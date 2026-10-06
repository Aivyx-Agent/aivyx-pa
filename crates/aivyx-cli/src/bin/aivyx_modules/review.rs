//! `aivyx-pa review` — steps that unattended runs (routines, webhooks, file
//! watches, the loop, team missions) parked for your approval, because their
//! area is at the `supervised` autonomy level. Approving runs the exact call
//! once; denying drops it. The render helper is pure so tests drive it
//! without IPC.

use std::io::{BufRead, Write};
use std::path::Path;

use aivyx_channel::daemon_client::{daemon_is_running, get_parked_steps, resolve_parked_step};
use aivyx_channel::daemon_ipc::default_socket_path;
use aivyx_channel::parked_steps::{ParkedState, ParkedStep};

/// How many resolved steps the list shows.
const RECENT: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum ReviewCommand {
    List,
    Approve { id: String, yes: bool },
    Deny { id: String },
}

/// Parse the words after `review`.
pub fn parse(args: &[String]) -> Result<ReviewCommand, String> {
    let usage = "usage: aivyx-pa review [list] | approve <id> [--yes] | deny <id>";
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["list"] => Ok(ReviewCommand::List),
        ["approve", id] => Ok(ReviewCommand::Approve { id: id.to_string(), yes: false }),
        ["approve", id, "--yes"] | ["approve", "--yes", id] => {
            Ok(ReviewCommand::Approve { id: id.to_string(), yes: true })
        }
        ["deny", id] => Ok(ReviewCommand::Deny { id: id.to_string() }),
        [other, ..] if !["list", "approve", "deny"].contains(other) => Err(format!(
            "unrecognized argument to `aivyx-pa review`: `{other}`\n{usage}"
        )),
        _ => Err(usage.to_string()),
    }
}

pub async fn run_review(cmd: ReviewCommand) -> Result<(), String> {
    let socket_path = default_socket_path()?;
    require_daemon_running(&socket_path).await?;
    match cmd {
        ReviewCommand::List => {
            let steps = get_parked_steps(&socket_path).await.map_err(|e| e.to_string())?;
            print!("{}", render_list(&steps, now_unix()));
        }
        ReviewCommand::Approve { id, yes } => {
            let steps = get_parked_steps(&socket_path).await.map_err(|e| e.to_string())?;
            let step = steps
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("no parked step with id `{id}`"))?;
            if step.state != ParkedState::Pending {
                return Err(format!("parked step `{id}` is already {}", step.state.as_str()));
            }
            println!("{}", step.summary);
            println!("  from {} — {}", step.origin, step.reason);
            if let Some(preview) = &step.preview {
                println!("{preview}");
            }
            if !yes && !confirm("Run this step now? [y/N] ")? {
                println!("Left for later.");
                return Ok(());
            }
            let done = resolve_parked_step(&socket_path, id, true)
                .await
                .map_err(|e| e.to_string())?;
            let result = done.result.unwrap_or_default();
            match done.state {
                ParkedState::Approved => println!("Approved — {result}"),
                _ => println!("Failed — {result}"),
            }
        }
        ReviewCommand::Deny { id } => {
            resolve_parked_step(&socket_path, id, false)
                .await
                .map_err(|e| e.to_string())?;
            println!("Denied.");
        }
    }
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn confirm(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// "5 min", "3 h", "2 days".
fn ago(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 3600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{} h", secs / 3600)
    } else {
        let d = secs / 86_400;
        format!("{d} {}", if d == 1 { "day" } else { "days" })
    }
}

/// Pending steps first, then the most recent resolved ones.
pub fn render_list(steps: &[ParkedStep], now: i64) -> String {
    let mut out = String::new();
    let pending: Vec<&ParkedStep> = steps.iter().filter(|s| s.state == ParkedState::Pending).collect();
    if pending.is_empty() {
        out.push_str("Nothing is waiting for your review.\n");
    } else {
        out.push_str("Waiting for your review:\n");
        for s in &pending {
            out.push_str(&format!(
                "  {}  {}\n        from {}, {} ago — {}\n",
                s.id,
                s.summary,
                s.origin,
                ago(now - s.parked_at),
                s.reason
            ));
        }
        out.push_str("Approve with `aivyx-pa review approve <id>`, or deny with `aivyx-pa review deny <id>`.\n");
    }
    let mut resolved: Vec<&ParkedStep> = steps.iter().filter(|s| s.state != ParkedState::Pending).collect();
    resolved.sort_by_key(|s| std::cmp::Reverse(s.resolved_at.unwrap_or(s.parked_at)));
    if !resolved.is_empty() {
        out.push_str("\nRecently resolved:\n");
        for s in resolved.iter().take(RECENT) {
            out.push_str(&format!("  {}  {:<8}  {}\n", s.id, s.state.as_str(), s.summary));
        }
    }
    out
}

async fn require_daemon_running(socket_path: &Path) -> Result<(), String> {
    if daemon_is_running(socket_path).await {
        return Ok(());
    }
    Err(format!(
        "aivyx-pa review: no daemon running on socket {} — \
         start the daemon first with `aivyx-pa daemon run`",
        socket_path.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, state: ParkedState, parked_at: i64, resolved_at: Option<i64>) -> ParkedStep {
        ParkedStep {
            id: id.into(),
            tool: "fs.delete".into(),
            input: serde_json::json!({"path": "old.txt"}),
            summary: format!("fs.delete {id}.txt"),
            reason: "deleting can't be undone".into(),
            area: "fs".into(),
            origin: "routine tidy".into(),
            trust_tier: aivyx_capability::TrustTier::Trusted,
            parked_at,
            state,
            resolved_at,
            result: None,
            preview: None,
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&argv(&[])), Ok(ReviewCommand::List));
        assert_eq!(parse(&argv(&["list"])), Ok(ReviewCommand::List));
        assert_eq!(
            parse(&argv(&["approve", "ab12cd34"])),
            Ok(ReviewCommand::Approve { id: "ab12cd34".into(), yes: false })
        );
        assert_eq!(
            parse(&argv(&["approve", "ab12cd34", "--yes"])),
            Ok(ReviewCommand::Approve { id: "ab12cd34".into(), yes: true })
        );
        assert_eq!(parse(&argv(&["deny", "ab12cd34"])), Ok(ReviewCommand::Deny { id: "ab12cd34".into() }));
        assert!(parse(&argv(&["approve"])).unwrap_err().starts_with("usage"));
        assert!(parse(&argv(&["bogus"])).unwrap_err().contains("`bogus`"));
    }

    #[test]
    fn an_empty_queue_says_so() {
        assert_eq!(render_list(&[], 0), "Nothing is waiting for your review.\n");
    }

    #[test]
    fn pending_then_recent_resolved() {
        let now = 100_000;
        let steps = vec![
            step("aaaa1111", ParkedState::Pending, now - 7200, None),
            step("bbbb2222", ParkedState::Approved, now - 9000, Some(now - 60)),
            step("cccc3333", ParkedState::Lapsed, now - 900_000, Some(now - 600)),
        ];
        let out = render_list(&steps, now);
        assert!(out.starts_with("Waiting for your review:\n  aaaa1111  fs.delete aaaa1111.txt\n        from routine tidy, 2 h ago — deleting can't be undone\n"), "{out}");
        let recent = out.split("Recently resolved:\n").nth(1).unwrap();
        assert!(recent.starts_with("  bbbb2222  approved  "), "{recent}");
        assert!(recent.contains("  cccc3333  lapsed    "), "{recent}");
    }
}
