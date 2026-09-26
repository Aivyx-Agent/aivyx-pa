//! Opt-in real-API smoke test for the Anthropic reference provider.
//!
//! This test actually talks to `api.anthropic.com`. It is `#[ignore]`-d
//! by default so `cargo test` stays hermetic — run it manually when you
//! want to confirm the provider still works against the live API:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-ant-... \
//!     cargo test -p aivyx-llm --features provider-anthropic \
//!     --test anthropic_smoke -- --ignored --nocapture
//! ```
//!
//! The test is a deliberate minimum: one short prompt, `claude-haiku-4-5`,
//! a small `max_tokens` cap, no tools. It asserts we can drain the
//! stream, get a `FinalMessage` with non-empty text, and that usage
//! accounting is populated. If this passes the wire format is still
//! intact end-to-end — transport, SSE parser, state machine, and the
//! `chat_stream` -> `next_event` -> `finish` lifecycle.
//!
//! If `ANTHROPIC_API_KEY` is missing the test returns early rather than
//! failing, so running `--ignored` in a CI environment without the
//! secret won't break the build — it just skips quietly. The rationale:
//! this is a manual-run smoke test, not a gate, and a hard failure on
//! a missing env var buries the signal.

#![cfg(feature = "provider-anthropic")]

use aivyx_llm::anthropic::{AnthropicConfig, AnthropicProvider};
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStreamEvent};
use secrecy::SecretString;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "hits api.anthropic.com — requires ANTHROPIC_API_KEY, run with --ignored"]
async fn final_message_against_real_api() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("ANTHROPIC_API_KEY not set — skipping smoke test");
        return;
    };

    let provider = AnthropicProvider::new(AnthropicConfig::new(SecretString::from(key)))
        .expect("AnthropicProvider::new should build a reqwest client");

    let messages = vec![LlmMessage::user_text(
        "Reply with exactly the single word: pong",
    )];
    let tools = [];
    let request = LlmRequest {
        model: "claude-haiku-4-5-20251001",
        system: Some("You are a terse test fixture. Obey the user's instruction exactly."),
        messages: &messages,
        tools: &tools,
        max_tokens: 16,
        temperature: Some(0.0),
        id_slot: None,
        slot_hint: None,
        route: None,
    };

    let cancel = CancellationToken::new();
    let mut stream = provider
        .chat_stream(request, &cancel)
        .await
        .expect("chat_stream should succeed against the real API");

    // Drain mid-stream events, reassembling text as we go. The provider
    // yields one TextChunk per `content_block_delta` of type text_delta.
    let mut reassembled = String::new();
    while let Some(event) = stream.next_event().await.expect("stream error") {
        match event {
            LlmStreamEvent::TextChunk(chunk) => reassembled.push_str(&chunk),
            LlmStreamEvent::Usage(_) => {}
        }
    }

    let terminal = stream
        .finish()
        .await
        .expect("finish should produce an LlmStepEnd");

    match terminal {
        LlmStepEnd::FinalMessage { text, usage } => {
            assert!(
                !text.is_empty(),
                "expected non-empty assistant text, got empty"
            );
            assert_eq!(
                reassembled, text,
                "reassembled TextChunks should equal terminal text"
            );
            assert!(
                usage.input_tokens > 0,
                "usage.input_tokens should be populated, got {}",
                usage.input_tokens
            );
            assert!(
                usage.output_tokens > 0,
                "usage.output_tokens should be populated, got {}",
                usage.output_tokens
            );
            eprintln!(
                "smoke test OK: text={:?} in={} out={}",
                text, usage.input_tokens, usage.output_tokens
            );
        }
        other => panic!("expected FinalMessage, got {other:?}"),
    }
}
