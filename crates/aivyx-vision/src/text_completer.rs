//! Adapts `aivyx-llm`'s `LlmProvider` to `aivyx-vision-svg`'s
//! `TextCompleter` seam, and builds the real provider from this
//! tool-process's own config (Task 2) -- deliberately separate from
//! whichever provider the daemon itself is using, since a tool process
//! runs in its own OS process and can't share the daemon's in-memory
//! provider.

use std::sync::Arc;

use aivyx_llm::{LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStreamEvent};
use aivyx_vision_svg::{TextCompleter, TextCompleterError};
use tokio_util::sync::CancellationToken;

use crate::config::{ProviderChoice, VisionConfig};

#[derive(Debug, thiserror::Error)]
pub enum BuildProviderError {
    #[error("failed to construct the {provider} provider: {source}")]
    Provider { provider: String, source: LlmError },
}

/// Construct the real `LlmProvider` this config selects.
pub fn build_provider(config: &VisionConfig) -> Result<Arc<dyn LlmProvider>, BuildProviderError> {
    match &config.provider {
        ProviderChoice::Ollama { base_url } => {
            let mut ollama_config = aivyx_llm::ollama::OllamaConfig::default_local();
            if let Some(url) = base_url {
                ollama_config = ollama_config.with_base_url(url.clone());
            }
            let provider = aivyx_llm::ollama::OllamaProvider::new(ollama_config).map_err(|e| {
                BuildProviderError::Provider {
                    provider: "ollama".to_string(),
                    source: e,
                }
            })?;
            Ok(Arc::new(provider))
        }
        ProviderChoice::Anthropic { api_key } => {
            let anthropic_config = aivyx_llm::anthropic::AnthropicConfig::new(api_key.clone());
            let provider =
                aivyx_llm::anthropic::AnthropicProvider::new(anthropic_config).map_err(|e| {
                    BuildProviderError::Provider {
                        provider: "anthropic".to_string(),
                        source: e,
                    }
                })?;
            Ok(Arc::new(provider))
        }
        ProviderChoice::Openai { api_key } => {
            let openai_config = aivyx_llm::openai::OpenAiConfig::new(api_key.clone());
            let provider = aivyx_llm::openai::OpenAiProvider::new(openai_config).map_err(|e| {
                BuildProviderError::Provider {
                    provider: "openai".to_string(),
                    source: e,
                }
            })?;
            Ok(Arc::new(provider))
        }
    }
}

/// Adapts a real `Arc<dyn LlmProvider>` to `TextCompleter` by issuing a
/// single-turn, tool-free, non-cancellable completion and buffering the
/// full text response.
pub struct LlmTextCompleter {
    provider: Arc<dyn LlmProvider>,
    model: String,
    max_tokens: u32,
}

impl LlmTextCompleter {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String, max_tokens: u32) -> Self {
        Self {
            provider,
            model,
            max_tokens,
        }
    }
}

#[async_trait::async_trait]
impl TextCompleter for LlmTextCompleter {
    async fn complete(&self, prompt: &str) -> Result<String, TextCompleterError> {
        let messages = vec![LlmMessage::user_text(prompt)];
        let request = LlmRequest {
            model: &self.model,
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: self.max_tokens,
            temperature: None,
            id_slot: None,
            slot_hint: None,
        };
        // One-shot, non-interactive call -- nothing to cancel from a
        // human-facing loop, so a token that's never triggered is
        // correct here, not a placeholder.
        let cancellation = CancellationToken::new();
        let mut stream = self
            .provider
            .chat_stream(request, &cancellation)
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        let mut text = String::new();
        while let Some(event) = stream
            .next_event()
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?
        {
            if let LlmStreamEvent::TextChunk(chunk) = event {
                text.push_str(&chunk);
            }
        }
        stream
            .finish()
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{
        LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    };
    use tokio_util::sync::CancellationToken;

    /// A fake `LlmProvider` streaming a canned sequence of text chunks,
    /// for testing the adapter without a real backend. A `Vec` (rather
    /// than one `String`) so tests can prove `complete()` concatenates
    /// every chunk in order, not just returns whichever chunk happens to
    /// be emitted -- a single-chunk fake could pass even if `complete()`
    /// kept only the last chunk instead of accumulating all of them.
    struct FakeProvider {
        chunks: Vec<String>,
    }

    struct FakeStream {
        remaining: std::collections::VecDeque<String>,
    }

    #[async_trait::async_trait]
    impl LlmStream for FakeStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(self.remaining.pop_front().map(LlmStreamEvent::TextChunk))
        }

        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: LlmUsage::default(),
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for FakeProvider {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(FakeStream {
                remaining: self.chunks.iter().cloned().collect(),
            }))
        }
    }

    #[tokio::test]
    async fn complete_returns_the_provider_s_full_text() {
        let provider: Arc<dyn LlmProvider> = Arc::new(FakeProvider {
            chunks: vec!["<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".to_string()],
        });
        let completer = LlmTextCompleter::new(provider, "test-model".to_string(), 2048);
        let result = completer.complete("draw a circle").await.unwrap();
        assert_eq!(result, "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
    }

    #[tokio::test]
    async fn complete_concatenates_multiple_streamed_chunks_in_order() {
        let provider: Arc<dyn LlmProvider> = Arc::new(FakeProvider {
            chunks: vec![
                "<svg xmlns=\"".to_string(),
                "http://www.w3.org/2000/svg\">".to_string(),
                "<circle r=\"5\"/>".to_string(),
                "</svg>".to_string(),
            ],
        });
        let completer = LlmTextCompleter::new(provider, "test-model".to_string(), 2048);
        let result = completer.complete("draw a circle").await.unwrap();
        assert_eq!(
            result,
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>"
        );
    }
}
