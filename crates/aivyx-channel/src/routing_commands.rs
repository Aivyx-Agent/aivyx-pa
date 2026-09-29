//! Routing visibility B2 — `/models` (list, `refresh`, `why`) and
//! `/model <id>|auto`, aivyx-pa's chat surface for the model router.
//! Ported from `aivyx-coder`'s `aivyx-core/src/routing_commands.rs`
//! (`format_models`/`render_residency`/`resolve`), adapted to this
//! codebase's `aivyx_llm::RoutedProvider` / `aivyx_route::Router`. Every
//! user-facing string is identical to `aivyx-coder`'s except the
//! routing-off reply, which names this codebase's config section
//! (spec `docs/superpowers/specs/2026-09-29-routing-visibility-design.md`,
//! B2).
//!
//! One module produces the reply for both interception points, so they
//! can't drift:
//! - the daemon's whole-message interception (`daemon_server.rs`, next to
//!   `/allow-cloud`) — never reaches the agent, the conversation history,
//!   or the model, and is available from any channel (reading and pinning
//!   aren't escalation);
//! - the in-process REPL's interception (`session.rs`, next to its own
//!   `/allow-cloud` reply) — only when `[routing] enabled = true` gave it
//!   a `RoutedProvider` to introspect.
//!
//! Model resolution reuses `aivyx_core::resolve_model` (the same resolver
//! `SetRoutingPin` calls) rather than a second copy of `aivyx-coder`'s
//! `resolve`.

use aivyx_llm::RoutedProvider;
use aivyx_route::{Availability, ModelKey, ModelProfile, ResidencyNote, ResidencySnapshot};

/// Shown when routing commands are asked for but there's no
/// `RoutedProvider` to answer them: `[routing] enabled` is false or
/// absent (or, in-process, routing wasn't threaded through). The one
/// wording difference from `aivyx-coder`'s `ROUTING_OFF`, which names its
/// own config instead.
pub const ROUTING_OFF_REPLY: &str =
    "Model routing commands are not available here — they need `[routing] enabled = true`.";

/// `None` when `input` is not a routing command — a normal turn, passed
/// through to the model as usual. An unrecognized `/models <arg>` is
/// likewise not a command (`/models please` is a normal turn, unlike
/// `aivyx-coder`'s `/models`, which replies "Unknown argument" for any
/// trailing text); an unrecognized `/model <arg>` IS a command — any
/// string may be a model id, so it's always an attempt to resolve one,
/// same as `aivyx-coder`.
pub async fn run(routed: Option<&RoutedProvider>, session: &str, input: &str) -> Option<String> {
    if let Some(arg) = parse_slash_command(input, "/models") {
        let Some(routed) = routed else {
            return Some(ROUTING_OFF_REPLY.to_string());
        };
        let router = routed.router();
        return match arg {
            "refresh" => Some(match routed.refresh().await {
                Ok(n) => format!("Refreshed: {n} candidate models."),
                Err(e) => format!("Could not refresh: {e}."),
            }),
            "why" => Some(match router.last_decision(session) {
                Some(r) => format!("`{}` ({}): {}", r.model, r.task.name(), r.reason),
                None => "No routed call yet in this conversation.".to_string(),
            }),
            "" => Some(format_models(
                &router.profiles(),
                router.current(session).as_ref(),
                router.pinned(session).as_ref(),
                &router.residency(),
            )),
            _ => None,
        };
    }
    let arg = parse_slash_command(input, "/model")?;
    let Some(routed) = routed else {
        return Some(ROUTING_OFF_REPLY.to_string());
    };
    let router = routed.router();
    Some(match arg {
        "" => match router.pinned(session) {
            Some(k) => format!("Pinned to `{k}`. Use /model auto to let routing choose again."),
            None => {
                "Not pinned; routing chooses this conversation's model. Use /model <id> to pin."
                    .to_string()
            }
        },
        "auto" => {
            router.unpin(session);
            "Pin cleared; routing chooses this conversation's model again on the next call."
                .to_string()
        }
        arg => match aivyx_core::resolve_model(&router.profiles(), arg) {
            Ok(key) => {
                router.pin(session, key.clone());
                format!("Pinned this conversation to `{key}`.")
            }
            Err(e) => e,
        },
    })
}

/// Is `text` exactly `name` (a whole message, surrounding whitespace
/// aside), or `name` followed by an argument after whitespace?
/// `/modelsx` is not `/models` (no boundary), and this is exactly the
/// "whole message only" rule the spec requires for these commands, as
/// for `/allow-cloud`.
fn parse_slash_command<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix(name)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// One line per candidate: `* ` marks the conversation's current model;
/// unknown capabilities carry a `?`. Appends a `Residency:` block when
/// `residency` has anything to say. Ported verbatim (in wording) from
/// `aivyx-coder`'s `format_models`.
pub fn format_models(
    profiles: &[ModelProfile],
    current: Option<&ModelKey>,
    pinned: Option<&ModelKey>,
    residency: &ResidencySnapshot,
) -> String {
    let mut out = String::from("Routing candidates (* = this conversation's model):");
    for p in profiles {
        let key = p.key();
        let marker = if current == Some(&key) { "* " } else { "  " };
        let mut caps: Vec<String> = p.capabilities.iter().map(ToString::to_string).collect();
        caps.extend(p.unknown_capabilities.iter().map(|c| format!("{c}?")));
        let ctx = p
            .context_window
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        let availability = match p.availability {
            Availability::Available => "available",
            Availability::Unverified => "unverified",
            Availability::Unavailable => "unavailable",
        };
        out.push_str(&format!(
            "\n{marker}{key} — {}, {}, ctx {ctx}, {availability}",
            p.tier,
            if caps.is_empty() {
                "no known capabilities".to_string()
            } else {
                caps.join(" ")
            },
        ));
        if pinned == Some(&key) {
            out.push_str(" (pinned)");
        }
    }
    out.push_str(&render_residency(residency, profiles));
    out
}

/// The `/models` residency block. An empty snapshot (no residency source
/// configured, or none has answered yet) adds nothing. Pure. Ported
/// verbatim from `aivyx-coder`'s `render_residency`.
fn render_residency(snapshot: &ResidencySnapshot, profiles: &[ModelProfile]) -> String {
    let mut loaded = Vec::new();
    let mut needs_load = Vec::new();
    let mut wont_fit = Vec::new();
    for p in profiles {
        match snapshot.cost(p).1 {
            Some(ResidencyNote::Loaded) => loaded.push(p.key().to_string()),
            Some(ResidencyNote::NeedsLoad) => needs_load.push(p.key().to_string()),
            Some(ResidencyNote::WontFit) => wont_fit.push(p.key().to_string()),
            None => {}
        }
    }
    let mut body = String::new();
    if !loaded.is_empty() {
        body.push_str(&format!("\n  loaded: {}", loaded.join(", ")));
    }
    if !needs_load.is_empty() {
        body.push_str(&format!("\n  needs load: {}", needs_load.join(", ")));
    }
    if !wont_fit.is_empty() {
        body.push_str(&format!("\n  may not fit: {}", wont_fit.join(", ")));
    }
    if let Some(vram) = snapshot.vram {
        let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
        let available = snapshot.available_vram().unwrap_or(0);
        body.push_str(&format!(
            "\nVRAM: {:.1} GiB used of {:.1} GiB ({:.1} GiB available for a load)",
            gib(vram.used_bytes),
            gib(vram.total_bytes),
            gib(available)
        ));
    }
    if body.is_empty() {
        return String::new();
    }
    format!("\n\nResidency:{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use aivyx_llm::{
        LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent,
        LlmUsage, ProviderFactory, RouteHint,
    };
    use aivyx_route::{Capability, EndpointRef, Router, TaskKind, TaskOverrides, Tier, Vram};
    use async_trait::async_trait;
    // `aivyx_llm::LlmProvider::chat_stream` takes `tokio_util::sync::
    // CancellationToken`; `aivyx_core` re-exports the same type.
    use aivyx_core::CancellationToken as TokioCancellationToken;

    fn profile(endpoint: &str, id: &str, tier: Tier) -> ModelProfile {
        let mut p = ModelProfile::new(id, EndpointRef::new(endpoint));
        p.tier = tier;
        p.capabilities.insert(Capability::Completion);
        p
    }

    fn key(endpoint: &str, id: &str) -> ModelKey {
        ModelKey {
            endpoint: EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    /// Answers every call with an empty successful stream — the model is
    /// never actually asked anything by these tests, but `RoutedProvider`
    /// needs a default provider to construct.
    struct Never;

    #[async_trait]
    impl LlmProvider for Never {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &TokioCancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(EmptyStream))
        }
    }

    struct EmptyStream;

    #[async_trait]
    impl LlmStream for EmptyStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: LlmUsage::default(),
            })
        }
    }

    fn router() -> RoutedProvider {
        let factory: ProviderFactory =
            Box::new(|_: &EndpointRef| Ok(Arc::new(Never) as Arc<dyn LlmProvider>));
        RoutedProvider::new(
            key("backend", "default"),
            Arc::new(Never),
            Router::new(
                vec![
                    profile("backend", "default", Tier::Medium),
                    profile("a-gpu", "qwen3:8b", Tier::Small),
                    profile("b-gpu", "qwen3:8b", Tier::Small),
                    profile("a-gpu", "coder", Tier::Large),
                ],
                TaskOverrides::default(),
            ),
            factory,
        )
    }

    /// One routed `Chat` call tagged with `session`, so `/models why` has
    /// something to report.
    async fn route_once(routed: &RoutedProvider, session: &str) {
        let messages = [LlmMessage::user_text("hi")];
        let request = LlmRequest {
            model: "default",
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: 8,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: Some(RouteHint {
                task: TaskKind::Chat,
                session: Some(session.to_string()),
                estimated_prompt_tokens: 0,
            }),
        };
        let stream = routed
            .chat_stream(request, &TokioCancellationToken::new())
            .await
            .unwrap();
        stream.finish().await.unwrap();
    }

    #[test]
    fn format_models_marks_current_and_pinned() {
        let mut coder = profile("a-gpu", "coder", Tier::Large);
        coder.capabilities.insert(Capability::Tools);
        coder.unknown_capabilities.insert(Capability::Vision);
        coder.context_window = Some(32_768);
        let text = format_models(
            &[profile("backend", "default", Tier::Medium), coder],
            Some(&key("a-gpu", "coder")),
            Some(&key("a-gpu", "coder")),
            &ResidencySnapshot::default(),
        );
        let line = text.lines().find(|l| l.contains("coder@a-gpu")).unwrap();
        assert!(line.starts_with("* "), "{line}");
        assert!(line.contains("large"), "{line}");
        assert!(line.contains("tools"), "{line}");
        assert!(line.contains("vision?"), "{line}");
        assert!(line.contains("ctx 32768"), "{line}");
        assert!(line.contains("pinned"), "{line}");
        let other = text
            .lines()
            .find(|l| l.contains("default@backend"))
            .unwrap();
        assert!(other.starts_with("  "), "{other}");
        assert!(other.contains("ctx ?"), "{other}");
        assert!(!text.contains("Residency"), "{text}");
    }

    #[test]
    fn format_models_shows_residency_when_the_snapshot_has_anything_to_say() {
        use aivyx_route::ModelResidency;

        let profiles = [
            profile("backend", "default", Tier::Medium),
            profile("a-gpu", "qwen3:8b", Tier::Small),
        ];
        let mut residency = ResidencySnapshot::default();
        residency.models.insert(
            key("backend", "default"),
            ModelResidency::Loaded {
                vram_bytes: Some(4 << 30),
            },
        );
        residency.models.insert(
            key("a-gpu", "qwen3:8b"),
            ModelResidency::NotLoaded {
                size_bytes: Some(30 << 30),
            },
        );
        residency.vram = Some(Vram {
            total_bytes: 24 << 30,
            used_bytes: 4 << 30,
        });
        let text = format_models(&profiles, None, None, &residency);
        assert!(text.contains("Residency:"), "{text}");
        assert!(text.contains("loaded: default@backend"), "{text}");
        assert!(text.contains("may not fit: qwen3:8b@a-gpu"), "{text}");
        assert!(
            text.contains("VRAM: 4.0 GiB used of 24.0 GiB (24.0 GiB available for a load)"),
            "{text}"
        );
    }

    #[test]
    fn format_models_adds_nothing_when_the_snapshot_matches_no_candidate() {
        let profiles = [profile("backend", "default", Tier::Medium)];
        let mut residency = ResidencySnapshot::default();
        residency.models.insert(
            key("other-gpu", "unrelated"),
            aivyx_route::ModelResidency::Loaded { vram_bytes: None },
        );
        let text = format_models(&profiles, None, None, &residency);
        assert!(!text.contains("Residency"), "{text}");
    }

    #[tokio::test]
    async fn non_commands_and_unrecognized_models_argument_pass_through() {
        assert_eq!(run(Some(&router()), "s", "hello").await, None);
        assert_eq!(run(Some(&router()), "s", "/modelsx").await, None);
        // Unlike aivyx-coder, an unrecognized `/models` argument is not a
        // command here — it's a normal turn.
        assert_eq!(run(Some(&router()), "s", "/models please").await, None);
    }

    #[tokio::test]
    async fn routing_off_names_this_codebases_config() {
        let text = run(None, "s", "/models").await.unwrap();
        assert!(text.contains("[routing] enabled = true"), "{text}");
        let text = run(None, "s", "/model coder").await.unwrap();
        assert_eq!(text, ROUTING_OFF_REPLY);
    }

    #[tokio::test]
    async fn model_pins_and_auto_unpins() {
        let r = router();
        let text = run(Some(&r), "s", "/model coder").await.unwrap();
        assert!(text.contains("coder@a-gpu"), "{text}");
        assert_eq!(r.router().pinned("s"), Some(key("a-gpu", "coder")));
        let text = run(Some(&r), "s", "/model auto").await.unwrap();
        assert_eq!(
            text,
            "Pin cleared; routing chooses this conversation's model again on the next call."
        );
        assert_eq!(r.router().pinned("s"), None);
        let text = run(Some(&r), "s", "/model").await.unwrap();
        assert_eq!(
            text,
            "Not pinned; routing chooses this conversation's model. Use /model <id> to pin."
        );
        let text = run(Some(&r), "s", "/model qwen3:8b").await.unwrap();
        assert!(text.contains("several endpoints"), "{text}");
        assert_eq!(r.router().pinned("s"), None);
    }

    #[tokio::test]
    async fn models_why_and_refresh() {
        let r = router();
        let text = run(Some(&r), "s", "/models why").await.unwrap();
        assert!(text.contains("No routed call yet"), "{text}");
        route_once(&r, "s").await;
        let text = run(Some(&r), "s", "/models why").await.unwrap();
        assert!(!text.contains("No routed call yet"), "{text}");
        assert!(text.contains("(chat):"), "{text}");
        // No discovery configured for this fixture.
        let text = run(Some(&r), "s", "/models refresh").await.unwrap();
        assert!(text.contains("Could not refresh"), "{text}");
        let text = run(Some(&r), "s", "/models").await.unwrap();
        assert!(text.contains("coder@a-gpu"), "{text}");
    }
}
