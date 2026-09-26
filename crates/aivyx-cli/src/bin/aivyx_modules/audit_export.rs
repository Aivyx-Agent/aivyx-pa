//! Phase 105 — `aivyx-pa audit export` JSONL emitter.
//!
//! Read-only offline export of the encrypted audit chain. The
//! HMAC-chained log carries everything a research-grade
//! trajectory exporter needs (per-turn / per-tool-call rows
//! with structured outcomes, capability snapshots, token
//! usage, scope-denials); Phase 105 just plumbs those rows out
//! as JSONL on stdout so downstream tooling — `jq`, a training
//! pipeline, a forensic auditor — can consume them without
//! reaching into `redb` by hand.
//!
//! Q-block resolutions (Phase 105):
//!
//! - **Q1(a)** — each line is the full `SignedEntry` projection
//!   (`seq + appended_at_ms + prev_mac (hex) + mac (hex) +
//!   event`) so downstream can re-verify the HMAC chain.
//! - **Q2(a)** — sequence-based filters only: `--from <seq>`
//!   and `--limit <N>` map directly to
//!   `PersistentAuditLog::entries_range`.
//! - **Q3(a)** — offline-only via the same cold-start storage
//!   open path `aivyx-pa --verify-only` uses; no new IPC variant,
//!   no daemon dependency, passphrase required.
//!
//! ## Re-verifying the chain downstream
//!
//! The MAC bytes per line are computed against the previous
//! entry's MAC concatenated with the canonical-JSON encoding
//! of the event. The genesis seed is *not* emitted in the
//! export (Q1 explicitly rejected the leading-header form);
//! downstream re-verify needs the seed supplied separately by
//! the operator. The HMAC algorithm and chain structure are
//! documented in `docs/DESIGN.md`'s Phase 6 audit-chain
//! section.

use std::io::Write as IoWrite;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aivyx_audit::{PersistentAuditLog, SignedEntry};
use aivyx_storage::Storage;
use serde::Serialize;

/// Default page size for the inner pagination loop. Used when
/// `limit` is `None` — we still want to read in bounded chunks
/// so a multi-gigabyte chain doesn't materialize as one
/// `Vec<SignedEntry>`.
const DEFAULT_PAGE_SIZE: usize = 1024;

// ---------------------------------------------------------------------------
// Per-line wire shape
// ---------------------------------------------------------------------------

/// One line of the JSONL export. Fields are kept flat (no
/// nesting under a "signed" key) so a `jq` filter like
/// `.event.kind == "ToolCall"` works without an extra
/// indirection.
///
/// `prev_mac` and `mac` are lowercase-hex strings — JSON does
/// not carry raw bytes cleanly, and hex round-trips through
/// every tool we expect downstream consumers to use.
#[derive(Debug, Clone, Serialize)]
struct ExportLine<'a> {
    seq: u64,
    appended_at_ms: u64,
    prev_mac: String,
    mac: String,
    event: &'a aivyx_audit::AuditEvent,
}

/// Render one `SignedEntry` as a single-line JSON object with
/// a trailing newline. Pure function — no I/O, no allocations
/// beyond the returned `String`.
///
/// Returns an error only when the underlying `AuditEvent`
/// serialization fails, which is structurally impossible for
/// well-formed entries (every variant is `Serialize` with
/// flat fields). Surfacing the error preserves the option of
/// adding non-`Serialize` future variants without silently
/// dropping lines.
pub fn render_line(entry: &SignedEntry) -> Result<String, serde_json::Error> {
    let line = ExportLine {
        seq: entry.seq,
        appended_at_ms: systemtime_to_ms(entry.appended_at),
        prev_mac: hex_encode(&entry.prev_mac),
        mac: hex_encode(&entry.mac),
        event: &entry.event,
    };
    let mut out = serde_json::to_string(&line)?;
    out.push('\n');
    Ok(out)
}

/// Convert a `SystemTime` to milliseconds since the Unix
/// epoch, saturating at `u64::MAX` on far-future timestamps
/// and clamping to `0` on pre-1970 timestamps (neither should
/// appear in a real chain — every audit entry is appended at
/// `SystemTime::now()` — but neither needs to crash the
/// emitter either).
fn systemtime_to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Lowercase-hex encoder. Avoids pulling in a `hex` dep for
/// 32 bytes of one-shot work.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Stream the audit chain to `writer` as JSONL.
///
/// Opens the chain via `PersistentAuditLog::open` against the
/// supplied storage handle and audit-chain key, then walks the
/// chain in `DEFAULT_PAGE_SIZE`-sized batches via
/// `entries_range`, emitting each entry as one JSONL line. The
/// chain is *not* held open as a live log — once the export
/// completes, the `PersistentAuditLog` is dropped, no drain
/// task lingers.
///
/// `from` and `limit` have the same semantics the CLI exposes:
/// `None` for `from` means "start at seq 0," `None` for
/// `limit` means "no upper bound." Both clamp gracefully when
/// the chain is shorter than requested.
///
/// Returns an error on storage, chain-replay, or serialization
/// failure. A broken chain returns the underlying
/// `AuditError::ChainBroken` wrapped in a string — same
/// fail-closed posture as `--verify-only`.
pub async fn export_chain(
    storage: Arc<dyn Storage>,
    audit_chain_key: [u8; 32],
    from: Option<u64>,
    limit: Option<usize>,
    event_type_filter: Option<&str>,
    writer: &mut dyn IoWrite,
) -> Result<usize, String> {
    let log = PersistentAuditLog::open(storage, audit_chain_key)
        .await
        .map_err(|e| format!("failed to open audit chain: {e}"))?;

    let mut cursor = from.unwrap_or(0);
    let remaining_cap = limit;
    let mut emitted: usize = 0;

    loop {
        let page_size = match remaining_cap {
            Some(cap) => std::cmp::min(DEFAULT_PAGE_SIZE, cap.saturating_sub(emitted)),
            None => DEFAULT_PAGE_SIZE,
        };
        if page_size == 0 {
            break;
        }

        let batch = log
            .entries_range(cursor, page_size)
            .map_err(|e| format!("audit-chain read failed at seq={cursor}: {e}"))?;
        if batch.is_empty() {
            break;
        }

        for entry in &batch {
            // Phase 113 — optional `--event-type` filter. The
            // entry walks through the existing render path
            // (so cursor advancement stays unchanged); the
            // write is skipped if the event-type label
            // doesn't match. `emitted` reflects what made it
            // to the writer, not what was scanned.
            if let Some(want) = event_type_filter {
                if event_type_label(&entry.event) != want {
                    continue;
                }
            }
            let line = render_line(entry)
                .map_err(|e| format!("failed to serialize entry seq={}: {e}", entry.seq))?;
            writer
                .write_all(line.as_bytes())
                .map_err(|e| format!("write failure on stdout: {e}"))?;
            emitted += 1;
        }

        cursor = batch.last().map(|e| e.seq + 1).unwrap_or(cursor);

        if let Some(cap) = remaining_cap {
            if emitted >= cap {
                break;
            }
        }
    }

    writer
        .flush()
        .map_err(|e| format!("flush failure on stdout: {e}"))?;
    Ok(emitted)
}

/// Phase 113 — return the stable string label for an
/// `AuditEvent` variant, mirroring the labelling switch the
/// daemon uses for the `event_type` field of the
/// `GetAuditEvents` IPC view. Kept here next to the export
/// path so the CLI's `--event-type` filter matches what the
/// operator sees in the JSONL output's `kind` discriminator.
pub fn event_type_label(event: &aivyx_audit::AuditEvent) -> &'static str {
    use aivyx_audit::AuditEvent;
    match event {
        AuditEvent::ToolCall { .. } => "ToolCall",
        AuditEvent::ScopeDenied { .. } => "ScopeDenied",
        AuditEvent::RateLimited { .. } => "RateLimited",
        AuditEvent::TurnStarted { .. } => "TurnStarted",
        AuditEvent::TurnEnded { .. } => "TurnEnded",
        AuditEvent::LlmCost { .. } => "LlmCost",
        AuditEvent::ModelRouted { .. } => "ModelRouted",
        AuditEvent::ConversationTainted { .. } => "ConversationTainted",
        AuditEvent::MemoryAccess { .. } => "MemoryAccess",
        AuditEvent::AutoNotifyDispatched { .. } => "AutoNotifyDispatched",
        AuditEvent::SkillAutoProposal { .. } => "SkillAutoProposal",
        AuditEvent::SkillInvocation { .. } => "SkillInvocation",
        AuditEvent::ProfileHintApplied { .. } => "ProfileHintApplied",
        AuditEvent::RoleDraftImported { .. } => "RoleDraftImported",
        AuditEvent::HeadlessRefusal { .. } => "HeadlessRefusal",
        AuditEvent::ConfigChanged { .. } => "ConfigChanged",
        AuditEvent::PersonaSeeded { .. } => "PersonaSeeded",
        AuditEvent::DocumentMutated { .. } => "DocumentMutated",
        AuditEvent::ScheduleMutated { .. } => "ScheduleMutated",
        AuditEvent::TeamMissionChannelTriggered { .. } => "TeamMissionChannelTriggered",
        AuditEvent::TeamMissionChannelDenied { .. } => "TeamMissionChannelDenied",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_audit::{AuditEvent, MemoryOperation, TrustTierSummary};
    use aivyx_capability::{CapabilitySet, Scope};
    use aivyx_core::{ChannelPlatform, SessionId, TurnId};
    use std::time::{Duration, UNIX_EPOCH};

    // -- hex_encode ------------------------------------------------------

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_known_bytes() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x10, 0xab]), "00ff10ab");
    }

    #[test]
    fn hex_encode_round_trip_via_lower_hex() {
        // 32-byte boundary is what the chain emits.
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
        let s = hex_encode(&bytes);
        assert_eq!(s.len(), 64);
        assert_eq!(&s[..8], "00010203");
        assert_eq!(&s[s.len() - 8..], "1c1d1e1f");
    }

    // -- systemtime_to_ms ------------------------------------------------

    #[test]
    fn systemtime_to_ms_unix_epoch_is_zero() {
        assert_eq!(systemtime_to_ms(UNIX_EPOCH), 0);
    }

    #[test]
    fn systemtime_to_ms_known_offset() {
        // 1_700_000_000.500 s after epoch -> 1_700_000_000_500 ms.
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_500);
        assert_eq!(systemtime_to_ms(t), 1_700_000_000_500);
    }

    // -- render_line -----------------------------------------------------

    fn signed_entry_with(seq: u64, event: AuditEvent) -> SignedEntry {
        SignedEntry {
            seq,
            appended_at: UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + seq * 1000),
            event,
            prev_mac: [0; 32],
            mac: [0; 32],
        }
    }

    fn sample_turn_started() -> AuditEvent {
        AuditEvent::TurnStarted {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            channel: ChannelPlatform::Local,
            trust_tier: TrustTierSummary::SemiTrusted,
            effective_capabilities: CapabilitySet::empty(),
        }
    }

    fn sample_memory_access() -> AuditEvent {
        AuditEvent::MemoryAccess {
            turn_id: TurnId::new(),
            operation: MemoryOperation::Read,
            scope: Scope::parse("memory.read").expect("memory.read is a known scope base"),
            query_or_key: "project-vision".to_string(),
        }
    }

    #[test]
    fn render_line_round_trips_through_serde() {
        let entry = signed_entry_with(7, sample_turn_started());
        let line = render_line(&entry).unwrap();

        assert!(line.ends_with('\n'), "line should end with newline");
        assert_eq!(line.matches('\n').count(), 1, "exactly one newline");

        let trimmed = line.trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap();

        assert_eq!(parsed["seq"], 7);
        assert_eq!(parsed["appended_at_ms"], 1_700_000_007_000_u64);
        assert_eq!(
            parsed["prev_mac"],
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            parsed["mac"],
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(parsed["event"]["kind"], "TurnStarted");
    }

    #[test]
    fn render_line_carries_event_kind_discriminator() {
        // Spot-check two distinct variants serialize with their
        // expected `kind` tag — the Q1(a) flat-fields requirement
        // (no nested `signed` wrapper) means downstream `jq`
        // filters like `.event.kind == "MemoryAccess"` must work.
        let mem = signed_entry_with(11, sample_memory_access());
        let line = render_line(&mem).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end_matches('\n')).unwrap();
        assert_eq!(v["event"]["kind"], "MemoryAccess");
        assert_eq!(v["event"]["query_or_key"], "project-vision");

        let turn = signed_entry_with(12, sample_turn_started());
        let line = render_line(&turn).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end_matches('\n')).unwrap();
        assert_eq!(v["event"]["kind"], "TurnStarted");
    }

    #[test]
    fn render_line_emits_lowercase_hex_for_mac_fields() {
        let mut entry = signed_entry_with(0, sample_turn_started());
        entry.prev_mac = [0xAB; 32];
        entry.mac = [0xCD; 32];
        let line = render_line(&entry).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end_matches('\n')).unwrap();
        assert_eq!(
            v["prev_mac"],
            "abababababababababababababababababababababababababababababababab"
        );
        assert_eq!(
            v["mac"],
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
        );
    }

    #[test]
    fn render_line_top_level_keys_are_flat() {
        // Q1(a) explicitly requires flat fields (no nested
        // `signed` envelope). Pin the key set so a future
        // refactor that wraps under a sub-object trips this
        // test instead of silently breaking downstream `jq`
        // filters.
        let entry = signed_entry_with(0, sample_turn_started());
        let line = render_line(&entry).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end_matches('\n')).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["appended_at_ms", "event", "mac", "prev_mac", "seq"]
        );
    }

    // -- export_chain pagination logic -----------------------------------
    //
    // The `export_chain` driver opens a live `PersistentAuditLog`
    // and drives `entries_range` against it. End-to-end exercise
    // belongs in an integration test against `RedbStorage`
    // (covered by the workspace's existing audit-chain
    // integration suite). Per-page emission logic stays simple
    // enough that the `render_line` round-trip coverage above
    // is the load-bearing assertion. If the cursor logic gains
    // complexity (e.g. resumable exports), a separate
    // fake-`AuditLog` test would be the right place to harden it.

    #[test]
    fn default_page_size_matches_documented_constant() {
        // Pin the constant — page-size regression here would
        // change the chunk shape downstream tools observe.
        assert_eq!(DEFAULT_PAGE_SIZE, 1024);
    }

    // -- Phase 113 — event_type_label ------------------------------------

    #[test]
    fn event_type_label_returns_stable_strings_for_existing_variants() {
        assert_eq!(event_type_label(&sample_turn_started()), "TurnStarted");
        assert_eq!(event_type_label(&sample_memory_access()), "MemoryAccess");
    }

    #[test]
    fn event_type_label_for_skill_auto_proposal_returns_kind_string() {
        let sap = AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: aivyx_audit::SkillAutoProposalOutcomeSummary::HeuristicGated,
            confidence_thousandths: None,
            proposed_skill_name: None,
            judge_latency_ms: None,
            heuristic_signals_matched: aivyx_audit::HeuristicSignalsMatched::default(),
            category: None,
            source: None,
        };
        assert_eq!(event_type_label(&sap), "SkillAutoProposal");
    }

    #[test]
    fn event_type_label_for_team_mission_channel_triggered() {
        // Review finding C1 — the missing arm broke compilation
        // entirely; this pins the label so a future variant addition
        // can't silently regress it the same way.
        let event = AuditEvent::TeamMissionChannelTriggered {
            platform: "telegram".to_string(),
            goal: "close the books".to_string(),
            mission_id: "m-1".to_string(),
        };
        assert_eq!(event_type_label(&event), "TeamMissionChannelTriggered");
    }
}
