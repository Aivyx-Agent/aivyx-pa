//! `aivyx-pa cost` — the usage report (Chapter K.4).
//!
//! Read-only, offline (the same cold-start storage open as `audit export` /
//! `--verify-only`: passphrase required, no session, no daemon). It scans the
//! HMAC chain for `AuditEvent::LlmCost` events — each carries the model + token
//! usage of one LLM-backed turn — and renders a [`CostReport`]: turns and
//! tokens in total and per model. `--today` scopes to the last 24h.
//!
//! aivyx is local-only, so runs cost nothing; a dollar column appears only
//! when the operator set a `[pricing.<model>]` rate.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aivyx_audit::{AuditEvent, PersistentAuditLog, SignedEntry};
use aivyx_core::TokenUsage;
use aivyx_cost::{CostReport, Pricing, TokenCounts};
use aivyx_storage::Storage;

const PAGE_SIZE: usize = 1024;
const DAY: Duration = Duration::from_secs(24 * 3600);

/// Map an audit `TokenUsage` to the cost crate's `TokenCounts`
/// (cache_creation ⇒ cache_write, cache_read ⇒ cache_read).
fn to_counts(u: &TokenUsage) -> TokenCounts {
    TokenCounts {
        input: u.input_tokens as u64,
        output: u.output_tokens as u64,
        cache_read: u.cache_read_input_tokens as u64,
        cache_write: u.cache_creation_input_tokens as u64,
    }
}

/// `aivyx-pa cost [--today]` — scan the chain's `LlmCost` events and print the
/// priced report. Offline; mirrors `run_audit_export`'s cold-start posture.
pub async fn run_cost(
    storage: Arc<dyn Storage>,
    audit_chain_key: [u8; 32],
    today: bool,
    pricing: Pricing,
) -> Result<(), String> {
    let log = PersistentAuditLog::open(storage, audit_chain_key)
        .await
        .map_err(|e| format!("failed to open audit chain: {e}"))?;

    let cutoff = today.then(|| SystemTime::now().checked_sub(DAY)).flatten();

    // Collect (model, counts) for every LlmCost event in the window.
    let mut entries: Vec<(String, TokenCounts)> = Vec::new();
    let mut cursor = 0u64;
    loop {
        let batch = log
            .entries_range(cursor, PAGE_SIZE)
            .map_err(|e| format!("audit-chain read failed at seq={cursor}: {e}"))?;
        if batch.is_empty() {
            break;
        }
        for entry in &batch {
            if let Some(pair) = llm_cost_in_window(entry, cutoff) {
                entries.push(pair);
            }
        }
        cursor = batch.last().map(|e| e.seq + 1).unwrap_or(cursor);
    }

    let report = CostReport::build(entries.iter().map(|(m, c)| (m.as_str(), *c)), &pricing);
    print!("{}", render_cost(&report, today));
    Ok(())
}

/// Extract `(model, counts)` from an entry iff it's an `LlmCost` event within
/// the window (`cutoff = None` ⇒ all time).
fn llm_cost_in_window(
    entry: &SignedEntry,
    cutoff: Option<SystemTime>,
) -> Option<(String, TokenCounts)> {
    let AuditEvent::LlmCost { model, usage, .. } = &entry.event else {
        return None;
    };
    if let Some(cutoff) = cutoff {
        if entry.appended_at < cutoff {
            return None;
        }
    }
    Some((model.clone(), to_counts(usage)))
}

/// Render a [`CostReport`] as an operator-readable block. Pure — the unit of
/// `aivyx-pa cost`.
pub fn render_cost(report: &CostReport, today: bool) -> String {
    let window = if today { "last 24h" } else { "all time" };
    if report.is_empty() {
        return format!(
            "Usage report — {window}\n  no model runs recorded yet.\n  (local models cost \
             nothing; runs land here once they happen.)\n"
        );
    }

    let priced = report.usd > 0.0;
    let mut out = format!("Usage report — {window}\n");
    out.push_str(&format!(
        "  total: {} turn{}, {} tokens{}\n\n",
        report.turns,
        if report.turns == 1 { "" } else { "s" },
        fmt_tokens(report.tokens.total()),
        if priced { format!(" ({} at your [pricing] rates)", usd(report.usd)) } else { String::new() },
    ));

    out.push_str(&format!("  {:<26}{:>7}{:>10}{:>10}", "model", "turns", "input", "output"));
    if priced {
        out.push_str(&format!("{:>12}", "cost"));
    }
    out.push('\n');
    for line in report.by_model.values() {
        out.push_str(&format!(
            "  {:<26}{:>7}{:>10}{:>10}",
            truncate(&line.model, 26),
            line.turns,
            fmt_tokens(line.tokens.input),
            fmt_tokens(line.tokens.output),
        ));
        if priced {
            out.push_str(&format!("{:>12}", usd(line.usd)));
        }
        out.push('\n');
    }
    out
}

fn usd(v: f64) -> String {
    format!("${v:.2}")
}

/// Compact token counts: `1.2M`, `340K`, `512`.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_cost::TokenCounts;

    fn io(i: u64, o: u64) -> TokenCounts {
        TokenCounts::new(i, o)
    }

    fn entries() -> Vec<(&'static str, TokenCounts)> {
        vec![
            ("qwen3.8-27b", io(1_000_000, 1_000_000)),
            ("qwen3.8-27b", io(500_000, 0)),
            ("llama3.1", io(2_000_000, 2_000_000)),
        ]
    }

    fn sample_report() -> CostReport {
        CostReport::build(entries(), &Pricing::new())
    }

    #[test]
    fn token_usage_maps_to_counts() {
        let u = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 7,
            cache_read_input_tokens: 3,
            ..Default::default()
        };
        let c = to_counts(&u);
        assert_eq!(c.input, 100);
        assert_eq!(c.output, 50);
        assert_eq!(c.cache_write, 7, "creation ⇒ write");
        assert_eq!(c.cache_read, 3);
    }

    #[test]
    fn renders_turns_and_tokens_without_dollars() {
        let out = render_cost(&sample_report(), false);
        assert!(out.contains("Usage report — all time"));
        assert!(out.contains("qwen3.8-27b"));
        assert!(out.contains("llama3.1"));
        assert!(out.contains("total: 3 turns, 6.5M tokens"), "{out}");
        assert!(!out.contains('$'), "no dollar figures without a rate;\n{out}");
        assert!(!out.contains("untracked"));
    }

    #[test]
    fn an_operator_rate_adds_a_cost_column() {
        let p = Pricing::with_overrides([(
            "llama3.1".to_string(),
            aivyx_cost::ModelRate::io(1.0, 1.0),
        )]);
        let out = render_cost(&CostReport::build(entries(), &p), false);
        assert!(out.contains("$4.00 at your [pricing] rates"), "{out}");
        assert!(out.contains("cost"));
    }

    #[test]
    fn today_window_label() {
        assert!(render_cost(&sample_report(), true).contains("last 24h"));
    }

    #[test]
    fn empty_report_renders_hint() {
        let empty = CostReport::build(Vec::<(&str, TokenCounts)>::new(), &Pricing::new());
        let out = render_cost(&empty, false);
        assert!(out.contains("no model runs recorded yet"));
    }

    #[test]
    fn token_formatting() {
        assert_eq!(fmt_tokens(512), "512");
        assert_eq!(fmt_tokens(2_400), "2K");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
    }
}
