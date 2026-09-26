//! Chapter Verdict — an LLM **acceptance judge** for autonomous-loop story
//! completion (backlog Opp E).
//!
//! Without `[loop] gate_command`, a story is marked `Done` purely on the agent's
//! self-reported `loop.complete` — the same "trust a self-report" hole the digest
//! fix (Chapter Ledger) closed for reporting. This adds an opt-in
//! `[loop] verify_completion`: at completion time, an independent LLM judge reads
//! the story's **acceptance criteria** (its `body`) and the agent's **summary**
//! of what it did, and renders PASS/FAIL. A FAIL blocks the completion — the
//! story stays `Pending` (no backlog reopen needed) and the agent is told why.
//!
//! **Scope (Chapter Verdict → #17b):** the judge checks the story's acceptance
//! criteria against the agent's *summary* AND — when a memory handle is wired
//! (`with_memory`) — a snapshot of the **recent memory the agent actually
//! wrote**. Grounding on the real artifact fixes the observed dogfood failure
//! where a genuinely-complete research story was rejected three times because
//! its summary was terse, even though the note was sitting in memory. The
//! summary alone could still be fabricated for artifact types the judge can't
//! see (files, test runs) — stack `gate_command` for those. The judge is a
//! quality layer, not a security gate, so it **fails open**: a judge LLM outage
//! logs and allows the completion rather than wedging the loop.

use std::sync::Arc;

use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};
use aivyx_memory::{is_internal_topic, Memory, MemoryEntry};

const JUDGE_SYSTEM: &str =
    "You are a strict, fair acceptance reviewer for an autonomous agent's work. \
     You are given a task (its title + acceptance criteria), the agent's own \
     summary of what it did, and — when available — snapshots of the recent \
     memory the agent wrote AND the recent files in its workspace. Decide \
     whether the acceptance criteria are met. Treat those snapshots as GROUND \
     TRUTH: if the memory OR a file shows the work was done (e.g. the required \
     note/file exists with the required content), PASS even when the agent's \
     summary is terse or vague. Be skeptical only when NEITHER the summary NOR \
     the evidence shows the criteria are satisfied — then FAIL. Reply with \
     EXACTLY one line, starting with the single word PASS or FAIL, then ' — ' \
     and a brief reason. Example: 'FAIL — neither the summary, memory, nor \
     workspace files show the required file was written.'";

const JUDGE_MAX_TOKENS: u32 = 256;

/// How many recent memory entries to show the judge as evidence, and the
/// per-entry body cap — enough to ground a story's artifact, bounded so the
/// judge prompt stays small.
const EVIDENCE_MAX_ENTRIES: usize = 12;
const EVIDENCE_BODY_CHARS: usize = 400;

/// File-artifact evidence bounds (the follow-on to #17b/#17d): the newest N
/// files under the agent's workspace, each capped, so a story that produced a
/// FILE ("save workspace-audit.md") is judged on what's on disk, not just the
/// summary. Bounded so a big workspace can't bloat the judge prompt.
const EVIDENCE_MAX_FILES: usize = 8;
const EVIDENCE_FILE_CHARS: usize = 600;
/// Cap on directory entries scanned, so a pathological workspace can't stall
/// the verdict.
const EVIDENCE_SCAN_CAP: usize = 2000;

/// The judge's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    pub reason: String,
}

/// An LLM acceptance judge. Owns its own provider + model (the binary builds it
/// from the daemon's provider when `[loop] verify_completion` is on).
pub struct CompletionJudge {
    provider: Arc<dyn LlmProvider>,
    model: String,
    /// #17b — optional memory handle. When set, `verify` snapshots the recent
    /// memory the agent wrote and shows it to the judge as ground-truth
    /// evidence, so a terse summary over real work no longer false-fails.
    memory: Option<Arc<dyn Memory>>,
    /// File-artifact grounding — the agent's workspace root. When set, `verify`
    /// also snapshots the most-recently-written files there, so a story that
    /// produced a FILE is judged on disk contents, not just the summary.
    workspace_root: Option<std::path::PathBuf>,
    /// Model routing Part 3a — set only via `with_route_task`, only when the
    /// daemon's provider is a `RoutedProvider` (routing is on). When set,
    /// `verify`'s request carries a `RouteHint` for this task so the router
    /// can pick the judge's model; `None` leaves `route: None` (untagged),
    /// matching every other provider's "untagged ⇒ unchanged" behavior.
    route_task: Option<aivyx_route::TaskKind>,
}

impl CompletionJudge {
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        CompletionJudge {
            provider,
            model: model.into(),
            memory: None,
            workspace_root: None,
            route_task: None,
        }
    }

    /// Tags every `verify()` call's request for model routing (only ever
    /// called when routing is on): the request carries `RouteHint { task,
    /// session: None, estimated_prompt_tokens }`. Not called ⇒ `route: None`,
    /// unchanged from pre-routing behavior.
    pub fn with_route_task(mut self, task: aivyx_route::TaskKind) -> Self {
        self.route_task = Some(task);
        self
    }

    /// Ground completion verdicts on the actual memory artifact (#17b): the
    /// judge is shown a snapshot of recent memory alongside the summary.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Ground verdicts on FILE artifacts too: the judge is shown the
    /// most-recently-written files under `root` (the agent's workspace).
    pub fn with_workspace(mut self, root: std::path::PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Judge whether `summary` satisfies the story's `title` + `criteria`.
    /// Fails **open**: an LLM error → `passed: true` (a judge outage must not
    /// wedge the loop), with the error noted in `reason`.
    pub async fn verify(&self, title: &str, criteria: &str, summary: &str) -> Verdict {
        let mut evidence_block = String::new();
        if let Some(e) = self.gather_memory_evidence().await {
            evidence_block.push_str(&format!(
                "\n\n## Recent memory the agent wrote (ground-truth evidence)\n{e}"
            ));
        }
        if let Some(f) = self.gather_file_evidence() {
            evidence_block.push_str(&format!(
                "\n\n## Recent workspace files (ground-truth evidence)\n{f}"
            ));
        }
        // Deterministic identifier backstop (live rig 2026-07-05): a mission
        // goal naming YPJT/YMML/YSSY was "verified" by an LLM verdict that
        // hallucinated PASS over a deliverable about entirely different
        // airports — after six correct rejections, the seventh sample let the
        // fiction through, and unbounded retries had guaranteed the false
        // PASS would eventually arrive. When the goal names ≥2 uppercase
        // identifiers and the MAJORITY of them appear nowhere in the summary
        // or the grounded evidence, no LLM opinion is needed: the deliverable
        // is about something else. Runs BEFORE the provider call, so this
        // class can't slip through the fail-open path either.
        let identifiers =
            extract_goal_identifiers(&format!("{title} {criteria}"));
        if identifiers.len() >= 2 {
            let haystack =
                format!("{summary}{evidence_block}").to_uppercase();
            let missing: Vec<&str> = identifiers
                .iter()
                .map(String::as_str)
                .filter(|i| !haystack.contains(*i))
                .collect();
            if missing.len() * 2 > identifiers.len() {
                return Verdict {
                    passed: false,
                    reason: format!(
                        "identifier check (deterministic): the goal names \
                         {} but the deliverable and evidence never mention \
                         {} — the artifact appears to address something \
                         else entirely",
                        identifiers.join(", "),
                        missing.join(", "),
                    ),
                };
            }
        }
        let user = format!(
            "## Task\nTitle: {title}\nAcceptance criteria:\n{}\n\n## Agent's summary of what it did\n{}{evidence_block}\n\n\
             Are the acceptance criteria met (by the summary OR the evidence)? Reply PASS or FAIL with a brief reason.",
            if criteria.trim().is_empty() { "(none given beyond the title)" } else { criteria },
            if summary.trim().is_empty() { "(the agent provided no summary)" } else { summary },
        );
        // Estimate = (system prompt chars + user prompt chars) / 4 — cheap
        // and consistent with the rest of model routing Part 3a; only
        // computed when routing tagged this judge at all.
        let route = self.route_task.clone().map(|task| aivyx_llm::RouteHint {
            task,
            session: None,
            estimated_prompt_tokens: ((JUDGE_SYSTEM.len() + user.len()) / 4) as u32,
        });
        let messages = vec![LlmMessage::user_text(user)];
        let request = LlmRequest {
            model: &self.model,
            system: Some(JUDGE_SYSTEM),
            messages: &messages,
            tools: &[],
            max_tokens: JUDGE_MAX_TOKENS,
            temperature: Some(0.0),
        id_slot: None,
        slot_hint: None,
        route,
        };
        let cancel = CancellationToken::new();
        let text = match self.provider.chat_stream(request, &cancel).await {
            Ok(mut stream) => {
                while let Ok(Some(_)) = stream.next_event().await {}
                match stream.finish().await {
                    Ok(LlmStepEnd::FinalMessage { text, .. }) => text,
                    Ok(_) => return fail_open("judge returned no final message"),
                    Err(e) => return fail_open(&format!("judge LLM error: {e}")),
                }
            }
            Err(e) => return fail_open(&format!("judge LLM error: {e}")),
        };
        parse_verdict(&text)
    }

    /// Snapshot the most-recent memory entries across all (non-internal)
    /// topics as ground-truth evidence for the judge. Best-effort: no memory
    /// handle, a store error, or an empty store ⇒ `None` (the judge falls
    /// back to summary-only). Newest-first, deduped nothing, bounded.
    async fn gather_memory_evidence(&self) -> Option<String> {
        let memory = self.memory.as_ref()?;
        let topics = memory.list_topics().await.ok()?;
        let mut entries: Vec<MemoryEntry> = Vec::new();
        for topic in topics {
            if is_internal_topic(&topic) {
                continue;
            }
            // A few newest per topic; the global sort+truncate below keeps the
            // overall most-recent set.
            if let Ok(mut es) = memory.get_recent(&topic, 3).await {
                entries.append(&mut es);
            }
        }
        if entries.is_empty() {
            return None;
        }
        // Most-recent first (global insertion order = seq; created_at breaks ties).
        entries.sort_by(|a, b| {
            b.created_at_secs
                .cmp(&a.created_at_secs)
                .then_with(|| b.seq.cmp(&a.seq))
        });
        entries.truncate(EVIDENCE_MAX_ENTRIES);
        let mut out = String::new();
        for e in &entries {
            let body: String = if e.body.chars().count() > EVIDENCE_BODY_CHARS {
                e.body.chars().take(EVIDENCE_BODY_CHARS).collect::<String>() + "…"
            } else {
                e.body.clone()
            };
            out.push_str(&format!("- [{}] {}\n", e.topic, body.replace('\n', " ")));
        }
        Some(out)
    }

    /// Snapshot the most-recently-modified files under the workspace as
    /// ground-truth evidence for file-producing stories. Best-effort: no
    /// workspace configured, or an unreadable/empty tree ⇒ `None`. Bounded on
    /// files scanned, files shown, and bytes per file; skips hidden/`.git`
    /// dirs and non-UTF-8 (binary) content.
    fn gather_file_evidence(&self) -> Option<String> {
        let root = self.workspace_root.as_ref()?;
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
        let mut stack = vec![root.clone()];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                if scanned >= EVIDENCE_SCAN_CAP {
                    break;
                }
                scanned += 1;
                let name = entry.file_name();
                // Skip hidden entries (.git, dotfiles) — noise, and the write
                // guard already keeps secrets out.
                if name.to_str().map(|n| n.starts_with('.')).unwrap_or(true) {
                    continue;
                }
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    files.push((mtime, path));
                }
            }
        }
        if files.is_empty() {
            return None;
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.0)); // newest first
        files.truncate(EVIDENCE_MAX_FILES);
        let mut out = String::new();
        for (_, path) in &files {
            let rel = path.strip_prefix(root).unwrap_or(path);
            // Read a bounded prefix; skip binary (non-UTF-8) content.
            let body = match std::fs::read(path) {
                Ok(bytes) => {
                    let capped =
                        &bytes[..bytes.len().min(EVIDENCE_FILE_CHARS * 2)];
                    // Salvage the valid UTF-8 prefix — a byte cap can split a
                    // multibyte char, so `valid_up_to()` recovers text that a
                    // naive `from_utf8` would reject. Only genuinely-binary
                    // content (no valid prefix) is flagged.
                    let valid = match std::str::from_utf8(capped) {
                        Ok(s) => s,
                        Err(e) if e.valid_up_to() > 0 => unsafe {
                            std::str::from_utf8_unchecked(&capped[..e.valid_up_to()])
                        },
                        Err(_) => "",
                    };
                    if valid.is_empty() {
                        "(binary file)".to_string()
                    } else {
                        let s: String =
                            valid.chars().take(EVIDENCE_FILE_CHARS).collect();
                        s.replace('\n', " ")
                    }
                }
                Err(_) => continue,
            };
            out.push_str(&format!("- [{}] {}\n", rel.display(), body));
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

/// Extract the goal's UPPERCASE identifier tokens — 3-8 chars of A-Z/0-9
/// with at least two letters (ICAO codes, tickers, part numbers…), deduped
/// in first-appearance order. Ordinary prose contributes nothing: a token
/// only matches when the goal's author deliberately wrote it in caps.
fn extract_goal_identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        let len = raw.len();
        if !(3..=8).contains(&len) {
            continue;
        }
        let all_upper_alnum = raw
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let letters = raw.chars().filter(|c| c.is_ascii_uppercase()).count();
        if all_upper_alnum && letters >= 2 && !out.iter().any(|o| o == raw) {
            out.push(raw.to_string());
        }
    }
    out
}

fn fail_open(reason: &str) -> Verdict {
    eprintln!("aivyx-pa loop: completion judge unavailable — allowing ({reason})");
    Verdict { passed: true, reason: format!("not verified ({reason})") }
}

/// Parse the judge's reply. The first non-empty line decides: a leading `FAIL`
/// → reject; a leading `PASS` → accept; anything else **fails open** (accept)
/// so a malformed verdict never blocks the loop. Pure + tested.
pub fn parse_verdict(text: &str) -> Verdict {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let upper = line.to_uppercase();
    if upper.starts_with("FAIL") {
        let reason = line.trim_start_matches(|c: char| c.is_alphabetic())
            .trim_start_matches([' ', '—', '-', ':'])
            .trim();
        Verdict {
            passed: false,
            reason: if reason.is_empty() { line.to_string() } else { reason.to_string() },
        }
    } else if upper.starts_with("PASS") {
        Verdict { passed: true, reason: line.to_string() }
    } else {
        // Ambiguous → fail open (don't block on a confused judge).
        Verdict { passed: true, reason: format!("unparseable verdict: {line}") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{LlmError, LlmStream, LlmStreamEvent, LlmUsage};
    use aivyx_memory::InMemoryMemory;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A provider that records the user prompt it was handed and replies with a
    /// fixed line — lets a test assert what the judge actually SAW.
    struct CapturingProvider {
        reply: String,
        seen: Arc<Mutex<String>>,
    }
    struct OneShot {
        text: Option<String>,
    }
    #[async_trait]
    impl LlmStream for OneShot {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: self.text.unwrap_or_default(),
                usage: LlmUsage::default(),
            })
        }
    }
    #[async_trait]
    impl LlmProvider for CapturingProvider {
        async fn chat_stream(
            &self,
            request: LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            // Capture the (single) user message text.
            if let Some(LlmMessage::User { content }) = request.messages.first() {
                let text: String = content
                    .iter()
                    .filter_map(|b| match b {
                        aivyx_llm::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                *self.seen.lock().unwrap() = text;
            }
            Ok(Box::new(OneShot { text: Some(self.reply.clone()) }))
        }
    }

    /// A provider that records the request's `route` hint — lets a test
    /// assert whether `with_route_task` actually tagged the outgoing request
    /// (model routing Part 3a).
    struct RouteCapturingProvider {
        route_seen: Arc<Mutex<Option<aivyx_llm::RouteHint>>>,
    }
    #[async_trait]
    impl LlmProvider for RouteCapturingProvider {
        async fn chat_stream(
            &self,
            request: LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            *self.route_seen.lock().unwrap() = request.route.clone();
            Ok(Box::new(OneShot { text: Some("PASS — ok".into()) }))
        }
    }

    #[test]
    fn extract_goal_identifiers_finds_deliberate_caps_only() {
        let ids = extract_goal_identifiers(
            "Check the current flight category and METAR at YPJT, YMML and \
             YSSY, then write a brief to the workspace as conditions-brief.md",
        );
        assert_eq!(ids, ["METAR", "YPJT", "YMML", "YSSY"]);
        // Ordinary prose (and short/lowercase tokens) contribute nothing.
        assert!(extract_goal_identifiers(
            "write a summary of the meeting to memory"
        )
        .is_empty());
        // Pure digits are not identifiers.
        assert!(extract_goal_identifiers("check 12345 and 678").is_empty());
    }

    #[tokio::test]
    async fn verify_rejects_deterministically_when_goal_identifiers_missing() {
        // The live rig failure (2026-07-05): the judge's SEVENTH sample
        // hallucinated PASS over a brief about entirely different airports.
        // The deterministic backstop must reject before any LLM opinion —
        // the provider here would say PASS if consulted.
        let seen = Arc::new(Mutex::new(String::new()));
        let judge = CompletionJudge::new(
            Arc::new(CapturingProvider {
                reply: "PASS — looks great".into(),
                seen: Arc::clone(&seen),
            }),
            "m",
        );
        let goal = "Check the current flight category and METAR at YPJT, \
                    YMML and YSSY and write a conditions brief";
        let summary = "## Conditions Brief\nKJFK: VFR, 10 SM. KLAX: IFR. \
                       KORD: VFR, broken 4000-6000.";
        let verdict = judge.verify(goal, goal, summary).await;
        assert!(!verdict.passed, "must reject: {}", verdict.reason);
        assert!(verdict.reason.contains("YPJT"), "{}", verdict.reason);
        // The provider was never consulted (fail-open can't rescue fiction).
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn verify_passes_identifier_check_when_deliverable_matches() {
        // A deliverable that DOES address the named identifiers sails through
        // to the LLM judge as before.
        let seen = Arc::new(Mutex::new(String::new()));
        let judge = CompletionJudge::new(
            Arc::new(CapturingProvider {
                reply: "PASS — brief covers all three airports".into(),
                seen: Arc::clone(&seen),
            }),
            "m",
        );
        let goal = "Check the current flight category and METAR at YPJT, \
                    YMML and YSSY and write a conditions brief";
        let summary = "YPJT VFR ceiling 4500; YMML VFR scattered 3400; \
                       YSSY showers, METAR retrieved for all three.";
        let verdict = judge.verify(goal, goal, summary).await;
        assert!(verdict.passed, "{}", verdict.reason);
        assert!(!seen.lock().unwrap().is_empty(), "LLM judge consulted");
    }

    #[tokio::test]
    async fn verify_grounds_on_recent_memory_evidence() {
        // The exact #17b failure: real work in memory, terse summary.
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put(
            "natural-therapies",
            "Melatonin (1–3 mg) shortens sleep onset; CBT-I reduces insomnia severity 30–50%.",
        )
        .await
        .unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let judge = CompletionJudge::new(
            Arc::new(CapturingProvider {
                reply: "PASS — the memory snapshot shows the required note.".into(),
                seen: Arc::clone(&seen),
            }),
            "test-model",
        )
        .with_memory(Arc::clone(&mem));

        let v = judge
            .verify(
                "Find 2 natural approaches to better sleep",
                "memory topic 'natural-therapies' names 2 approaches with a rationale each",
                "added two approaches", // terse — would fail summary-only
            )
            .await;
        assert!(v.passed, "verdict: {v:?}");
        // The judge actually saw the memory artifact as evidence.
        let prompt = seen.lock().unwrap().clone();
        assert!(prompt.contains("ground-truth evidence"), "evidence block present");
        assert!(prompt.contains("natural-therapies"), "topic in evidence");
        assert!(prompt.contains("Melatonin"), "artifact body in evidence");
    }

    #[tokio::test]
    async fn verify_grounds_on_workspace_file_evidence() {
        // A file-producing story: the artifact is on disk, summary is terse.
        let dir = std::env::temp_dir()
            .join(format!("aivyx-judge-fe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("workspace-audit.md"),
            "# Workspace audit\n- projects/\n- 2 recommendations: tidy journal, archive old plans.\n",
        )
        .unwrap();
        // A hidden file must be ignored (not shown as evidence).
        std::fs::write(dir.join(".secret"), "TOKEN=nope").unwrap();

        let seen = Arc::new(Mutex::new(String::new()));
        let judge = CompletionJudge::new(
            Arc::new(CapturingProvider {
                reply: "PASS — the workspace file shows the audit.".into(),
                seen: Arc::clone(&seen),
            }),
            "test-model",
        )
        .with_workspace(dir.clone());

        let v = judge
            .verify(
                "Audit the workspace and write a report",
                "workspace-audit.md exists with a tree summary + 2 recommendations",
                "did the audit", // terse — would fail summary-only
            )
            .await;
        assert!(v.passed, "verdict: {v:?}");
        let prompt = seen.lock().unwrap().clone();
        assert!(prompt.contains("workspace files"), "file evidence block present");
        assert!(prompt.contains("workspace-audit.md"), "file name in evidence");
        assert!(prompt.contains("recommendations"), "file body in evidence");
        assert!(!prompt.contains(".secret"), "hidden files excluded");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verify_omits_evidence_block_without_memory() {
        let seen = Arc::new(Mutex::new(String::new()));
        let judge = CompletionJudge::new(
            Arc::new(CapturingProvider {
                reply: "PASS — fine.".into(),
                seen: Arc::clone(&seen),
            }),
            "test-model",
        ); // no with_memory
        let _ = judge.verify("t", "c", "s").await;
        let prompt = seen.lock().unwrap().clone();
        assert!(!prompt.contains("ground-truth evidence"), "no evidence block");
    }

    #[tokio::test]
    async fn verify_tags_the_request_when_with_route_task_is_set() {
        // Model routing Part 3a — a judge built `with_route_task(Judge)`
        // sends a `RouteHint` for that task; a non-routed provider ignores
        // it (it's just an extra field), but a `RoutedProvider` reads it.
        let route_seen = Arc::new(Mutex::new(None));
        let judge = CompletionJudge::new(
            Arc::new(RouteCapturingProvider { route_seen: Arc::clone(&route_seen) }),
            "test-model",
        )
        .with_route_task(aivyx_route::TaskKind::Judge);
        let _ = judge.verify("t", "c", "s").await;
        let seen = route_seen.lock().unwrap().clone().expect("route hint present");
        assert_eq!(seen.task, aivyx_route::TaskKind::Judge);
        assert_eq!(seen.session, None);
        assert!(seen.estimated_prompt_tokens > 0, "got {seen:?}");
    }

    #[tokio::test]
    async fn verify_leaves_route_none_without_with_route_task() {
        // Untagged ⇒ unchanged: no `with_route_task` call ⇒ `route == None`.
        let route_seen = Arc::new(Mutex::new(None));
        let judge = CompletionJudge::new(
            Arc::new(RouteCapturingProvider { route_seen: Arc::clone(&route_seen) }),
            "test-model",
        );
        let _ = judge.verify("t", "c", "s").await;
        assert!(route_seen.lock().unwrap().is_none());
    }

    #[test]
    fn parses_fail_with_reason() {
        let v = parse_verdict("FAIL — the summary gives no evidence the file changed.");
        assert!(!v.passed);
        assert_eq!(v.reason, "the summary gives no evidence the file changed.");
    }

    #[test]
    fn parses_pass() {
        assert!(parse_verdict("PASS — looks complete, criteria met.").passed);
        assert!(parse_verdict("pass").passed);
    }

    #[test]
    fn fails_only_on_a_leading_fail() {
        // "fail" in the reason of a PASS must not flip it.
        assert!(parse_verdict("PASS — no failures found").passed);
        // leading FAIL (any case) rejects.
        assert!(!parse_verdict("fail: nope").passed);
    }

    #[test]
    fn ambiguous_fails_open() {
        // A judge that didn't follow the format must not block the loop.
        assert!(parse_verdict("I think it is probably fine").passed);
        assert!(parse_verdict("").passed);
    }
}
