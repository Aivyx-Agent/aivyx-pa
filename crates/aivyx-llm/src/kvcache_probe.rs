//! Fetches `total_slots` + `build_info` from a real llama-server `/props`
//! response -- the one piece of server metadata `LlmPlanner`'s kvcache
//! wiring needs at startup to size the `KvSlotPool` and detect a server
//! upgrade (via `build_info`, folded into `CacheKey.build_hash`).
//!
//! `aivyx-pa` has no other `/props`-consuming code today (unlike
//! `aivyx-coder`, which already probes `/props` for context-window
//! detection) -- this is a standalone fetch, not an extension of an
//! existing one.

use std::time::Duration;

/// Bounds the `/props` fetch below -- an unbounded client here was a
/// real (not hypothetical) startup-hang risk found the hard way during
/// `aivyx-coder`'s own kvcache adoption: a server that accepts the TCP
/// connection but doesn't answer until a large model finishes loading
/// blocks forever without one.
pub const KVCACHE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaSlotsInfo {
    pub total_slots: u32,
    pub build_info: String,
}

/// Pure parser over an already-fetched `/props` JSON body -- no I/O,
/// independently testable without a real server.
pub fn parse_llama_slots_info(json: &serde_json::Value) -> Option<LlamaSlotsInfo> {
    let total_slots = json.get("total_slots")?.as_u64()? as u32;
    let build_info = json.get("build_info")?.as_str()?.to_string();
    Some(LlamaSlotsInfo { total_slots, build_info })
}

/// Fetches and parses `{base_url}/props` in one call. `base_url` must
/// already be the bare origin (no `/v1` suffix) -- `/props`, like
/// `/slots`, is a native llama-server endpoint, not an OpenAI-compat one.
/// Fully fail-open: any failure (build, network, timeout, non-2xx,
/// malformed body) returns `None`, never propagates an error.
///
/// Requires the `provider-openai` feature (which gates reqwest availability).
#[cfg(feature = "provider-openai")]
pub async fn fetch_llama_slots_info(base_url: &str) -> Option<LlamaSlotsInfo> {
    let client = crate::transport::client_builder_for(base_url)
        .timeout(KVCACHE_PROBE_TIMEOUT)
        .build()
        .ok()?;
    let url = format!("{}/props", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    parse_llama_slots_info(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llama_slots_info_from_real_props_shape() {
        // Real /props response shape, confirmed against a live
        // llama-server during aivyx-coder's own adoption (2026-08-21).
        let json = serde_json::json!({
            "default_generation_settings": {"params": {}},
            "total_slots": 4,
            "model_path": "/home/me/models/model.gguf",
            "build_info": "b10107-3121043"
        });
        let info = parse_llama_slots_info(&json).expect("must parse a real llama-server /props body");
        assert_eq!(info.total_slots, 4);
        assert_eq!(info.build_info, "b10107-3121043");
    }

    #[test]
    fn parse_llama_slots_info_returns_none_when_fields_are_absent() {
        let json = serde_json::json!({"some_other_server": true});
        assert!(parse_llama_slots_info(&json).is_none());
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn fetch_llama_slots_info_returns_none_when_nothing_is_listening() {
        // No server at all on this port -- confirms the fail-open path
        // returns None rather than panicking or hanging past the timeout.
        let result = fetch_llama_slots_info("http://127.0.0.1:1").await;
        assert!(result.is_none());
    }
}
