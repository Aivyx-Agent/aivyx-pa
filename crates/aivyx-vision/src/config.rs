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
//! provider = "ollama"   # "ollama" | "openai" (an OpenAI-compatible local server)
//! model = "qwen3:8b"
//!
//! # provider = "ollama": optional, defaults to localhost.
//! # provider = "openai": required.
//! # base_url = "http://127.0.0.1:11434"
//!
//! # Optional; only for an OpenAI-compatible server that wants one.
//! # api_key = "sk-..."
//! ```
//!
//! 2.0 (local-only): every model URL here (`base_url`, `[mold]`
//! `broker_url` and `mold_url`) must pass aivyx-route's `require_local` at
//! load, and `provider = "anthropic"` was removed.

use std::io;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use thiserror::Error;

use aivyx_instance::InstancePaths;

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
    #[error("config file at {path:?}: {reason}")]
    Provider { path: PathBuf, reason: String },
    /// 2.0 (local-only): a model URL that is not on this machine or the
    /// operator's own network. `reason` is aivyx-route's `NotLocal` text.
    #[error("{key}: {reason}")]
    NotLocal { key: String, reason: String },
}

/// The removed-kind error for this tool process's own `provider` field.
pub fn removed_provider_message(kind: &str) -> String {
    format!(
        "provider \"{kind}\" was removed in 2.0 — aivyx is local-only; use ollama or openai \
         (an OpenAI-compatible local server) with a local base_url"
    )
}

#[derive(Deserialize)]
struct RawVisionConfig {
    provider: String,
    model: String,
    base_url: Option<String>,
    api_key: Option<String>,
    mold: Option<RawMoldConfig>,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for RawVisionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawVisionConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("mold", &self.mold)
            .finish()
    }
}

#[derive(Deserialize)]
struct RawMoldConfig {
    broker_url: String,
    mold_url: String,
    api_key: Option<String>,
    output_dir: Option<String>,
    /// The mold model, for example `flux2-klein:q8`; unset uses the image
    /// model downloaded on the mold server.
    model: Option<String>,
    /// Attach a small preview of each image to the result, so a model that
    /// can see images is shown what it made. Default on.
    show_model_previews: Option<bool>,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for RawMoldConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawMoldConfig")
            .field("broker_url", &self.broker_url)
            .field("mold_url", &self.mold_url)
            .field("api_key", &"<redacted>")
            .field("output_dir", &self.output_dir)
            .field("model", &self.model)
            .field("show_model_previews", &self.show_model_previews)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum ProviderChoice {
    Ollama { base_url: Option<String> },
    /// An OpenAI-compatible local server: `base_url` is required, the key
    /// optional.
    Openai { base_url: String, api_key: Option<SecretString> },
}

/// Image-generation backend config -- present only when the operator has
/// added a `[mold]` section to `config.toml`. Absent by default, so an
/// existing install with only the LLM-provider fields (for
/// `vision.generate_svg`) keeps working unchanged; `vision.generate_image`/
/// `vision.generate_3d` simply aren't registered until this is configured.
/// See `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`.
#[derive(Clone)]
pub struct MoldSettings {
    pub broker_url: String,
    pub mold_url: String,
    pub api_key: Option<String>,
    pub output_dir: PathBuf,
    pub model: Option<String>,
    pub show_model_previews: bool,
}

// Hand-written: the derived `Debug` would print the key (audit P14).
impl std::fmt::Debug for MoldSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoldSettings")
            .field("broker_url", &self.broker_url)
            .field("mold_url", &self.mold_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("output_dir", &self.output_dir)
            .field("model", &self.model)
            .field("show_model_previews", &self.show_model_previews)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider: ProviderChoice,
    pub model: String,
    pub mold: Option<MoldSettings>,
}

pub fn default_config_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("vision")
        .map(|d| d.join("config.toml"))
}

pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current().map_err(|_| ConfigFileError::NoHome)?;
    default_config_path_for(&instance).ok_or(ConfigFileError::NoHome)
}

/// Where generated image/3D-model files land when the operator doesn't
/// override `[mold] output_dir` explicitly -- matches the ecosystem
/// spec's own storage convention
/// (`~/.local/share/aivyx-pa/vision/<uuid>.<ext>` for default instance,
/// `~/.local/share/aivyx-pa/instances/<n>/vision/` for named instances);
/// `aivyx-vision-mold`'s own `MoldProvider` generates the `<uuid>.<ext>`
/// filename itself, so this only needs to point at the right directory).
pub fn default_output_dir() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current()
        .map_err(|_| ConfigFileError::NoHome)?;
    instance
        .home_data_dir()
        .map(|d| d.join("vision"))
        .ok_or(ConfigFileError::NoHome)
}

/// Loads and checks the config: every model URL must be local (system
/// resolver; blocking DNS for names that aren't local by spelling).
pub fn load_config(path: &Path) -> Result<VisionConfig, ConfigFileError> {
    load_config_with(path, &aivyx_route::SystemResolver)
}

/// [`load_config`] with an explicit resolver for the local-only check.
pub fn load_config_with(
    path: &Path,
    resolver: &dyn aivyx_route::Resolve,
) -> Result<VisionConfig, ConfigFileError> {
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

    let provider_error = |reason: String| ConfigFileError::Provider {
        path: path.to_path_buf(),
        reason,
    };
    let provider = match raw.provider.as_str() {
        "ollama" => ProviderChoice::Ollama {
            base_url: raw.base_url,
        },
        "openai" => ProviderChoice::Openai {
            base_url: raw.base_url.ok_or_else(|| {
                provider_error(
                    "provider \"openai\" means an OpenAI-compatible local server and needs \
                     base_url — set it to a server on this machine or your own network"
                        .to_string(),
                )
            })?,
            api_key: raw.api_key.map(SecretString::from),
        },
        "anthropic" => return Err(provider_error(removed_provider_message("anthropic"))),
        other => {
            return Err(provider_error(format!(
                "unknown provider {other:?}; supported: ollama, openai"
            )));
        }
    };
    let check = |key: &str, url: &str| {
        aivyx_route::require_local_with(url, resolver).map_err(|e| ConfigFileError::NotLocal {
            key: key.to_string(),
            reason: e.to_string(),
        })
    };
    match &provider {
        ProviderChoice::Ollama { base_url: Some(url) }
        | ProviderChoice::Openai { base_url: url, .. } => check("base_url", url)?,
        ProviderChoice::Ollama { base_url: None } => {}
    }
    if let Some(m) = &raw.mold {
        check("[mold] broker_url", &m.broker_url)?;
        check("[mold] mold_url", &m.mold_url)?;
    }

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
                model: m.model,
                show_model_previews: m.show_model_previews.unwrap_or(true),
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
    /// Audit P14: `Debug` of the Mold settings never prints the key.
    #[test]
    fn mold_debug_hides_the_key() {
        let m = MoldSettings {
            broker_url: "http://127.0.0.1:1".into(),
            mold_url: "http://127.0.0.1:2".into(),
            api_key: Some("MOLD-SECRET-KEY".into()),
            output_dir: std::path::PathBuf::from("/tmp/x"),
            model: None,
            show_model_previews: true,
        };
        let shown = format!("{m:?}");
        assert!(!shown.contains("MOLD-SECRET-KEY") && shown.contains("<redacted>"), "{shown}");
    }

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
    fn anthropic_was_removed_and_names_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"anthropic\"\nmodel = \"claude-3-5-sonnet-20241022\"\napi_key = \"k\"\n",
        )
        .unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::Provider { .. }), "{err:?}");
        assert!(
            err.to_string().ends_with(
                "provider \"anthropic\" was removed in 2.0 — aivyx is local-only; use ollama or \
                 openai (an OpenAI-compatible local server) with a local base_url"
            ),
            "{err}"
        );
    }

    #[test]
    fn loads_a_local_openai_compatible_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"openai\"\nmodel = \"qwen3\"\nbase_url = \"http://192.168.1.5:8000\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.model, "qwen3");
        assert!(matches!(
            config.provider,
            ProviderChoice::Openai { ref base_url, api_key: None } if base_url == "http://192.168.1.5:8000"
        ));
    }

    #[test]
    fn openai_without_base_url_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"openai\"\nmodel = \"gpt-4o\"\napi_key = \"sk\"\n")
            .unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::Provider { .. }), "{err:?}");
        assert!(err.to_string().contains("needs base_url"), "{err}");
    }

    /// Fixed DNS answers (no network in tests).
    struct Stub;
    impl aivyx_route::Resolve for Stub {
        fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<std::net::IpAddr>> {
            match host {
                "gpu.home.example" => Ok(vec!["10.0.0.4".parse().unwrap()]),
                "pub.example.com" => Ok(vec!["93.184.216.34".parse().unwrap()]),
                _ => Err(io::Error::other("no such host")),
            }
        }
    }

    #[test]
    fn every_model_url_must_be_local() {
        const TAIL: &str = " — aivyx is local-only; point it at a model server on this \
                            machine or your own network";
        let mold = |broker: &str, mold: &str| {
            format!(
                "provider = \"ollama\"\nmodel = \"m\"\n[mold]\nbroker_url = \"{broker}\"\n\
                 mold_url = \"{mold}\"\n"
            )
        };
        for (body, expected) in [
            (
                "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"https://api.openai.com\"\n"
                    .to_string(),
                format!("base_url: https://api.openai.com/ is not local (cloud host){TAIL}"),
            ),
            (
                "provider = \"ollama\"\nmodel = \"m\"\nbase_url = \"http://pub.example.com:11434\"\n"
                    .to_string(),
                format!(
                    "base_url: http://pub.example.com:11434/ is not local (resolves to a public \
                     address 93.184.216.34){TAIL}"
                ),
            ),
            (
                mold("http://8.8.8.8:8899", "http://127.0.0.1:7680"),
                format!(
                    "[mold] broker_url: http://8.8.8.8:8899/ is not local (resolves to a public \
                     address 8.8.8.8){TAIL}"
                ),
            ),
            (
                mold("http://127.0.0.1:8899", "http://nowhere.example:7680"),
                format!(
                    "[mold] mold_url: http://nowhere.example:7680/ is not local (doesn't \
                     resolve){TAIL}"
                ),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, &body).unwrap();
            let err = load_config_with(&path, &Stub).unwrap_err();
            assert!(matches!(err, ConfigFileError::NotLocal { .. }), "{err:?}");
            assert_eq!(err.to_string(), expected, "{body}");
        }
        // A LAN name that resolves local loads.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, mold("http://gpu.home.example:8899", "http://[::1]:7680")).unwrap();
        assert!(load_config_with(&path, &Stub).unwrap().mold.is_some());
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
             output_dir = \"/tmp/vision-out\"\n\
             model = \"flux2-klein:q8\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        let mold = config.mold.expect("mold section must be present");
        assert_eq!(mold.broker_url, "http://127.0.0.1:8899");
        assert_eq!(mold.mold_url, "http://127.0.0.1:7680");
        assert_eq!(mold.api_key.as_deref(), Some("secret"));
        assert_eq!(mold.output_dir, PathBuf::from("/tmp/vision-out"));
        assert_eq!(mold.model.as_deref(), Some("flux2-klein:q8"));
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

    #[test]
    fn default_config_path_for_named_instance() {
        use aivyx_instance::{BaseDirs, InstanceName};
        let dirs = BaseDirs {
            home: Some(PathBuf::from("/home/user")),
            xdg_config_home: Some(PathBuf::from("/etc/config")),
            xdg_data_home: Some(PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_config_path_for(&instance).expect("path");
        assert_eq!(
            p,
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/vision/config.toml")
        );
    }
}
