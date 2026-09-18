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
    #[error(
        "config file at {path:?} not found -- create it with an LLM provider/model (see aivyx-vision's own README)"
    )]
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
    mold: Option<RawMoldConfig>,
}

#[derive(Debug, Deserialize)]
struct RawMoldConfig {
    broker_url: String,
    mold_url: String,
    api_key: Option<String>,
    output_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ProviderChoice {
    Ollama { base_url: Option<String> },
    Anthropic { api_key: SecretString },
    Openai { api_key: SecretString },
}

/// Image-generation backend config -- present only when the operator has
/// added a `[mold]` section to `config.toml`. Absent by default, so an
/// existing install with only the LLM-provider fields (for
/// `vision.generate_svg`) keeps working unchanged; `vision.generate_image`/
/// `vision.generate_3d` simply aren't registered until this is configured.
/// See `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`.
#[derive(Debug, Clone)]
pub struct MoldSettings {
    pub broker_url: String,
    pub mold_url: String,
    pub api_key: Option<String>,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider: ProviderChoice,
    pub model: String,
    pub mold: Option<MoldSettings>,
}

pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let home = std::env::var_os("HOME").ok_or(ConfigFileError::NoHome)?;
    Ok(PathBuf::from(home)
        .join(".aivyx-pa")
        .join("tool-processes")
        .join("vision")
        .join("config.toml"))
}

/// Where generated image/3D-model files land when the operator doesn't
/// override `[mold] output_dir` explicitly -- matches the ecosystem
/// spec's own storage convention
/// (`~/.local/share/aivyx-pa/vision/<uuid>.<ext>`;
/// `aivyx-vision-mold`'s own `MoldProvider` generates the `<uuid>.<ext>`
/// filename itself, so this only needs to point at the right directory).
pub fn default_output_dir() -> Result<PathBuf, ConfigFileError> {
    let home = std::env::var_os("HOME").ok_or(ConfigFileError::NoHome)?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("aivyx-pa")
        .join("vision"))
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
            api_key: raw.api_key.map(SecretString::from).ok_or_else(|| {
                ConfigFileError::MissingApiKey {
                    provider: "anthropic".to_string(),
                }
            })?,
        },
        RawProvider::Openai => ProviderChoice::Openai {
            api_key: raw.api_key.map(SecretString::from).ok_or_else(|| {
                ConfigFileError::MissingApiKey {
                    provider: "openai".to_string(),
                }
            })?,
        },
    };

    let mold = raw
        .mold
        .map(|m| -> Result<MoldSettings, ConfigFileError> {
            let output_dir = match m.output_dir {
                Some(d) => PathBuf::from(d),
                None => default_output_dir()?,
            };
            Ok(MoldSettings {
                broker_url: m.broker_url,
                mold_url: m.mold_url,
                api_key: m.api_key,
                output_dir,
            })
        })
        .transpose()?;

    Ok(VisionConfig {
        provider,
        model: raw.model,
        mold,
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
        assert!(matches!(
            config.provider,
            ProviderChoice::Ollama { base_url: None }
        ));
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
        std::fs::write(
            &path,
            "provider = \"anthropic\"\nmodel = \"claude-3-5-sonnet-20241022\"\n",
        )
        .unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::MissingApiKey { .. }));
    }

    #[test]
    fn loads_a_valid_openai_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"openai\"\nmodel = \"gpt-4o\"\napi_key = \"sk-test\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.model, "gpt-4o");
        assert!(matches!(config.provider, ProviderChoice::Openai { .. }));
    }

    #[test]
    fn openai_without_api_key_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"openai\"\nmodel = \"gpt-4o\"\n").unwrap();
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

    #[test]
    fn loads_a_config_with_no_mold_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n").unwrap();
        let config = load_config(&path).unwrap();
        assert!(config.mold.is_none());
    }

    #[test]
    fn loads_a_config_with_a_full_mold_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n\n\
             [mold]\n\
             broker_url = \"http://127.0.0.1:8899\"\n\
             mold_url = \"http://127.0.0.1:7680\"\n\
             api_key = \"secret\"\n\
             output_dir = \"/tmp/vision-out\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        let mold = config.mold.expect("mold section must be present");
        assert_eq!(mold.broker_url, "http://127.0.0.1:8899");
        assert_eq!(mold.mold_url, "http://127.0.0.1:7680");
        assert_eq!(mold.api_key.as_deref(), Some("secret"));
        assert_eq!(mold.output_dir, PathBuf::from("/tmp/vision-out"));
    }

    #[test]
    fn loads_a_mold_section_with_no_api_key_or_output_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n\n\
             [mold]\n\
             broker_url = \"http://127.0.0.1:8899\"\n\
             mold_url = \"http://127.0.0.1:7680\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        let mold = config.mold.expect("mold section must be present");
        assert_eq!(mold.api_key, None);
        // Falls back to default_output_dir() -- just confirm it's the same
        // value that function computes, not a hardcoded literal here (this
        // test would need updating if $HOME weren't stable within one test
        // run, which it is).
        assert_eq!(mold.output_dir, default_output_dir().unwrap());
    }

    #[test]
    fn default_output_dir_lands_under_home_local_share_aivyx_pa_vision() {
        let dir = default_output_dir().unwrap();
        assert!(dir.ends_with("aivyx-pa/vision"));
        assert!(dir.starts_with(std::env::var("HOME").unwrap()));
    }
}
