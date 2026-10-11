//! MCP client adapter — Phase 23 Task 3, Phase 32 transport refactor.
//!
//! Bridges an MCP server's surface into Aivyx `Tool`s. Discovery is
//! **capability-gated** on the server's `initialize` response, covering
//! the three core MCP primitives:
//! - declared `tools` → tool proxies;
//! - declared `resources` → `mcp.<server>.resources.list` / `.read`
//!   (read the context a server exposes);
//! - declared `prompts` → `mcp.<server>.prompts.list` / `.get`
//!   (discover + retrieve reusable prompt templates).
//!
//! The client never probes a method the server didn't declare. Three
//! transports are supported: **stdio** (local child process), **SSE**
//! (the legacy HTTP+SSE pair), and **Streamable HTTP** (the modern
//! single-endpoint transport, MCP 2025-03-26+).
//!
//! The bridge's request loop demuxes server-initiated notifications
//! that interleave with responses; a `*/list_changed` is recorded as a
//! pending refresh (see [`McpServerBridge::take_pending_list_changed`]
//! / [`McpServerBridge::rediscover`]).

mod conn;
mod jsonrpc;
mod limits;
pub mod notifications;
mod prompt_proxy;
mod protocol;
mod proxy;
mod resource_proxy;
pub mod sse;
pub mod stdio;
pub mod streamable_http;
pub mod transport_trait;
mod transport;

pub use notifications::{classify_incoming, Incoming, ListKind};
pub use prompt_proxy::{McpPromptProxy, PromptTool};
pub use protocol::{McpPromptDef, McpResourceDef, PromptMessage, ResourceContents};
pub use proxy::McpToolProxy;
pub use resource_proxy::{McpResourceProxy, ResourceTool};
pub use sse::SseTransport;
pub use streamable_http::StreamableHttpTransport;
pub use stdio::{SandboxConfig, StderrLog, StdioTransport};
pub use transport::McpServerBridge;
pub use transport_trait::McpTransport;

/// Chapter Conduit (CD.2) — apply operator-supplied HTTP headers to a
/// request, skipping any whose name (case-insensitive) collides with a
/// protocol-reserved header. The operator can add auth (`Authorization`,
/// `X-Api-Key`, …) but can never clobber the MCP wire contract.
pub(crate) fn apply_operator_headers(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
    reserved: &[&str],
) -> reqwest::RequestBuilder {
    for (k, v) in headers {
        if reserved.iter().any(|r| r.eq_ignore_ascii_case(k)) {
            continue;
        }
        req = req.header(k, v);
    }
    req
}

/// A canned-response transport for unit tests: every `send` is recorded
/// and each `receive` pops the next pre-loaded reply, so a bridge can be
/// driven through `initialize` + method calls without a real server.
#[cfg(test)]
pub(crate) mod testutil {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::transport_trait::McpTransport;

    pub(crate) struct MockTransport {
        replies: Mutex<VecDeque<String>>,
    }

    impl MockTransport {
        /// `replies` are returned by successive `receive` calls, in order.
        pub(crate) fn new(replies: Vec<String>) -> Self {
            MockTransport {
                replies: Mutex::new(replies.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl McpTransport for MockTransport {
        async fn send(&self, _message: &str) -> Result<(), String> {
            Ok(())
        }

        async fn receive(&self) -> Result<String, String> {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "MockTransport: no more canned replies".to_string())
        }
    }
}
