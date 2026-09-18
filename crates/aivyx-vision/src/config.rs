//! Operator-supplied LLM provider configuration for `aivyx-vision`.
//!
//! Loaded once at tool-process startup from
//! `~/.aivyx-pa/tool-processes/vision/config.toml`. This tool process
//! runs as a separate OS process from the daemon, so it cannot share
//! the daemon's own in-memory `Arc<dyn LlmProvider>` -- it gets its own,
//! separately-configured one, the same way `aivyx-gmail` has its own
//! OAuth config rather than sharing the daemon's.
//!
//! ## File format
//!
//! ```toml
//! # ~/.aivyx-pa/tool-processes/vision/config.toml
//!
//! provider = "ollama"   # "ollama" | "anthropic" | "openai"
//! model = "qwen3:8b"
//!
//! # Only read when provider = "ollama"; omit for the default localhost.
//! # base_url = "http://127.0.0.1:11434"
//!
//! # Only read when provider = "anthropic" or "openai"; required for those.
//! # api_key = "sk-..."
//! ```

use std::io;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("config file I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("config file at {path:?} not found -- create it with an LLM provider/model (see aivyx-vision's own README)")]
    NotFound { path: PathBuf },
    #[error("config file at {path:?} failed to parse as TOML: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("$HOME is unset; cannot resolve default config path")]
    NoHome,
    #[error("provider {provider:?} requires an api_key, but none was set")]
    MissingApiKey { provider: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawProvider {
    Ollama,
    Anthropic,
    Openai,
}

#[derive(Debug, Deserialize)]
struct RawVisionConfig {
    provider: RawProvider,
    model: String,
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ProviderChoice {
    Ollama { base_url: Option<String> },
    Anthropic { api_key: SecretString },
    Openai { api_key: SecretString },
}

#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider: ProviderChoice,
    pub model: String,
}

pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let home = std::env::var_os("HOME").ok_or(ConfigFileError::NoHome)?;
    Ok(PathBuf::from(home)
        .join(".aivyx-pa")
        .join("tool-processes")
        .join("vision")
        .join("config.toml"))
}

pub fn load_config(path: &Path) -> Result<VisionConfig, ConfigFileError> {
    let raw_text = std::fs::read_to_string(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ConfigFileError::NotFound {
                path: path.to_path_buf(),
            }
        } else {
            ConfigFileError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    let raw: RawVisionConfig = toml::from_str(&raw_text).map_err(|e| ConfigFileError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;

    let provider = match raw.provider {
        RawProvider::Ollama => ProviderChoice::Ollama {
            base_url: raw.base_url,
        },
        RawProvider::Anthropic => ProviderChoice::Anthropic {
            api_key: raw
                .api_key
                .map(SecretString::from)
                .ok_or_else(|| ConfigFileError::MissingApiKey {
                    provider: "anthropic".to_string(),
                })?,
        },
        RawProvider::Openai => ProviderChoice::Openai {
            api_key: raw
                .api_key
                .map(SecretString::from)
                .ok_or_else(|| ConfigFileError::MissingApiKey {
                    provider: "openai".to_string(),
                })?,
        },
    };

    Ok(VisionConfig {
        provider,
        model: raw.model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_valid_ollama_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n").unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.model, "qwen3:8b");
        assert!(matches!(config.provider, ProviderChoice::Ollama { base_url: None }));
    }

    #[test]
    fn loads_a_valid_ollama_config_with_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"ollama\"\nmodel = \"qwen3:8b\"\nbase_url = \"http://127.0.0.1:9999\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert!(matches!(
            config.provider,
            ProviderChoice::Ollama { base_url: Some(ref u) } if u == "http://127.0.0.1:9999"
        ));
    }

    #[test]
    fn anthropic_without_api_key_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"anthropic\"\nmodel = \"claude-3-5-sonnet-20241022\"\n").unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::MissingApiKey { .. }));
    }

    #[test]
    fn missing_file_is_a_clear_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::NotFound { .. }));
    }

    #[test]
    fn malformed_toml_is_a_clear_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::Parse { .. }));
    }
}
