//! Unknown-key reporting for `config.toml`.
//!
//! The config types stay as they are: the TOML is deserialized through
//! `serde_ignored`, which hands back the path of every key the types did not
//! consume. Those become [`UnknownKey`]s, each with a "did you mean" hint
//! drawn from the stable-surface snapshot (`docs/stable/config.txt`), which
//! is checked against the types by a test and so lists every known key.
//! Free-form maps (`HashMap` fields) consume all their entries and are never
//! reported.

use std::collections::BTreeSet;
use std::fmt;

/// The snapshot of every known key path; one `path: type` per line.
/// `[]` marks an array of tables and `.*` a map of user-named entries.
const KNOWN_SNAPSHOT: &str = include_str!("../../../docs/stable/config.txt");

/// A key in the config file that this version does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    /// Full dotted path, e.g. `sandbox.allow_leaving_process_group`.
    pub path: String,
    /// The known key this one probably meant.
    pub suggestion: Option<String>,
}

impl fmt::Display for UnknownKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown config key `{}` (ignored)", self.path)?;
        if let Some(s) = &self.suggestion {
            write!(f, ": did you mean `{s}`?")?;
        }
        Ok(())
    }
}

/// Every known key path plus every section prefix of one. `[]` is dropped
/// (array of tables); `*` stays as the segment of a user-named map entry.
fn known_paths() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in KNOWN_SNAPSHOT.lines() {
        let Some((path, ty)) = line.split_once(": ") else { continue };
        // A deprecated spelling is never suggested; its new one is.
        if ty.starts_with("deprecated alias of ") {
            continue;
        }
        let mut prefix = String::new();
        for seg in path.split('.') {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(seg.trim_end_matches("[]"));
            out.insert(prefix.clone());
        }
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `serde_ignored` marks an `Option` layer with a `?` segment; drop it.
fn clean(path: &str) -> String {
    path.split('.').filter(|s| *s != "?").collect::<Vec<_>>().join(".")
}

fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// The nearest known key: within edit distance 2, else the same last segment
/// in another section. Array indices are ignored when comparing.
fn suggest(path: &str, known: &BTreeSet<String>, by_last_segment: bool) -> Option<String> {
    let norm: Vec<&str> = path.split('.').filter(|s| s.parse::<usize>().is_err()).collect();
    let norm = norm.join(".");
    let near = known
        .iter()
        .filter(|k| *k != &norm && !k.contains('*'))
        .map(|k| (edit_distance(&norm, k), k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d);
    if let Some((_, k)) = near {
        return Some(k.clone());
    }
    if by_last_segment {
        let last = last_segment(&norm);
        // Leaf keys only: a section named like the key is not a good hint.
        return known
            .iter()
            .find(|k| *k != &norm && !k.contains('*') && last_segment(k) == last && k.contains('.'))
            .cloned();
    }
    None
}

/// Expands an unknown table into its leaf keys so a misplaced section is
/// reported per key (`sandbox.allow_leaving_process_group`), not as one blob.
fn leaves(prefix: &str, v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) if !t.is_empty() => {
            for (k, v) in t {
                leaves(&format!("{prefix}.{k}"), v, out);
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

fn lookup<'a>(root: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut segs = path.split('.');
    let mut cur = root.get(segs.next()?)?;
    for s in segs {
        cur = match cur {
            toml::Value::Table(t) => t.get(s)?,
            toml::Value::Array(a) => a.get(s.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Keys `[routing]` hands to pa's own sub-tables (`RawRouting` flattens the
/// shared `RoutingConfig` beside them); the shared type ignores these.
const PA_ROUTING_SUBTABLES: [&str; 3] = ["escalation", "sensitive", "classifier"];

/// Parses `raw` as `T` and returns the unknown keys beside it. The value is
/// exactly what `toml::from_str` returns.
///
/// `[routing]` needs a second pass: `RawRouting` flattens
/// `aivyx_route::RoutingConfig` (which ignores unknown keys) beside pa's own
/// sub-tables, and `serde_ignored` cannot see through `#[serde(flatten)]`.
/// So the main pass's `routing.*` reports are dropped and the `[routing]`
/// table is checked on its own against `RoutingConfig`, skipping pa's
/// sub-tables (which deny unknown keys themselves, as a hard error).
///
/// Deprecated spellings are moved to their new paths first
/// ([`crate::deprecated_keys`]); the third value is one warning per
/// deprecated spelling found.
pub(crate) fn parse_reporting<T: serde::de::DeserializeOwned>(
    raw: &str,
) -> Result<(T, Vec<UnknownKey>, Vec<String>), toml::de::Error> {
    let mut table = raw.parse::<toml::Table>().ok();
    let deprecated = table.as_mut().map(crate::deprecated_keys::rewrite).unwrap_or_default();
    let mut ignored = Vec::new();
    let value: T = if deprecated.is_empty() {
        serde_ignored::deserialize(toml::Deserializer::new(raw), |p| {
            ignored.push(clean(&p.to_string()));
        })?
    } else {
        // The file as written is parsed first, so an error in it is reported
        // with its real line and column (an old spelling is just ignored by
        // this pass). Then the rewritten table is parsed for the values; an
        // error now can only come from a moved value, so it is reported
        // without the position, which would point into the rewritten text.
        toml::from_str::<T>(raw)?;
        let rewritten = toml::to_string(table.as_ref().expect("rewrite ran on a parsed table"))
            .map_err(<toml::de::Error as serde::de::Error>::custom)?;
        serde_ignored::deserialize(toml::Deserializer::new(&rewritten), |p| {
            ignored.push(clean(&p.to_string()));
        })
        .map_err(|e| <toml::de::Error as serde::de::Error>::custom(e.message()))?
    };
    ignored.retain(|p| p != "routing" && !p.starts_with("routing."));
    if let Some(routing) = table.as_ref().and_then(|t| t.get("routing")).cloned() {
        let mut second = Vec::new();
        // A malformed `[routing]` already failed the main pass.
        let _ = serde_ignored::deserialize::<_, _, aivyx_route::RoutingConfig>(routing, |p| {
            second.push(clean(&p.to_string()));
        });
        ignored.extend(
            second
                .into_iter()
                .filter(|p| !PA_ROUTING_SUBTABLES.contains(&p.split('.').next().unwrap_or("")))
                .map(|p| format!("routing.{p}")),
        );
    }
    let known = known_paths();
    let mut paths = Vec::new();
    for p in ignored {
        match table.as_ref().and_then(|t| lookup(t, &p)) {
            // A whole unknown section: a near-miss on the section name wins;
            // otherwise report each key in it.
            Some(v @ toml::Value::Table(_)) if suggest(&p, &known, false).is_none() => {
                leaves(&p, v, &mut paths);
            }
            _ => paths.push(p),
        }
    }
    let mut seen = BTreeSet::new();
    let keys = paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .map(|path| {
            let suggestion = suggest(&path, &known, true);
            UnknownKey { path, suggestion }
        })
        .collect();
    Ok((value, keys, deprecated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RawToml;

    fn unknown(raw: &str) -> Vec<UnknownKey> {
        parse_reporting::<RawToml>(raw).unwrap().1
    }

    fn paths(raw: &str) -> Vec<String> {
        unknown(raw).into_iter().map(|k| k.path).collect()
    }

    #[test]
    fn unknown_top_level_key_warns() {
        assert_eq!(paths("bogus_key = 1\n"), vec!["bogus_key"]);
    }

    #[test]
    fn unknown_nested_key_warns_with_its_full_path() {
        assert_eq!(paths("[agent]\nmystery = true\n"), vec!["agent.mystery"]);
    }

    #[test]
    fn near_miss_gets_a_suggestion() {
        let keys = unknown("[access]\nlevl = \"read\"\n");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].suggestion.as_deref(), Some("access.level"));
        assert_eq!(
            keys[0].to_string(),
            "unknown config key `access.levl` (ignored): did you mean `access.level`?"
        );
    }

    #[test]
    fn coders_sandbox_key_is_known() {
        // R7: coder's spelling, from its troubleshooting page, now works in pa.
        assert!(unknown("[sandbox]\nallow_leaving_process_group = true\n").is_empty());
    }

    #[test]
    fn a_deprecated_spelling_is_not_also_an_unknown_key() {
        let (_, keys, deprecated) =
            parse_reporting::<RawToml>("[confine]\nallow_unix_sockets = true\n").unwrap();
        assert!(keys.is_empty(), "{keys:?}");
        assert_eq!(deprecated.len(), 1);
    }

    #[test]
    fn a_deprecated_spelling_is_never_suggested() {
        let keys = unknown("[confine]\nrequire_enforcment = true\n");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].path, "confine.require_enforcment");
        assert_ne!(keys[0].suggestion.as_deref(), Some("confine.require_enforcement"));
    }

    #[test]
    fn misspelled_section_is_reported_once_with_a_suggestion() {
        let keys = unknown("[sandobx]\nrequire_enforcement = true\n");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].path, "sandobx");
        assert_eq!(keys[0].suggestion.as_deref(), Some("sandbox"));
    }

    #[test]
    fn valid_config_has_no_warnings() {
        let raw = "[agent]\nmodel = \"m\"\n[access]\nlevel = \"workspace\"\n\
                   [sandbox]\nrequire_enforcement = true\n\
                   [routing]\nenabled = true\n";
        assert!(unknown(raw).is_empty(), "{:?}", unknown(raw));
    }

    #[test]
    fn free_form_map_entries_do_not_warn() {
        let raw = "[routing]\nenabled = true\n\
                   [routing.endpoints.mybox]\nkind = \"ollama\"\nbase_url = \"http://x\"\n\
                   [routing.tasks.coding]\ntier = \"medium\"\n\
                   [[mcp_server]]\nname = \"x\"\ncommand = \"y\"\n\
                   [mcp_server.env]\nANY_NAME = \"1\"\n";
        assert!(unknown(raw).is_empty(), "{:?}", unknown(raw));
    }

    #[test]
    fn unknown_key_in_the_flattened_routing_section_warns_once() {
        let raw = "[routing]\nenabled = true\ndiscovr = false\n";
        let keys = unknown(raw);
        let got: Vec<&str> = keys.iter().map(|k| k.path.as_str()).collect();
        assert_eq!(got, vec!["routing.discovr"]);
        assert_eq!(keys[0].suggestion.as_deref(), Some("routing.discover"));
    }

    #[test]
    fn pas_own_routing_subtables_are_not_reported_as_unknown() {
        let raw = "[routing]\nenabled = true\n[routing.escalation]\nmode = \"never\"\n\
                   [routing.sensitive]\nchannels = []\n[routing.classifier]\nenabled = false\n";
        assert!(unknown(raw).is_empty(), "{:?}", unknown(raw));
    }

    #[test]
    fn unknown_key_inside_an_array_of_tables_warns() {
        let raw = "[[mcp_server]]\nname = \"x\"\ncommand = \"y\"\nbogus = 1\n";
        assert_eq!(paths(raw), vec!["mcp_server.0.bogus"]);
    }

    #[test]
    fn loaded_values_are_unchanged_by_the_warning_path() {
        let raw = "stray = 1\n[agent]\nmodel = \"m\"\nstray2 = 2\n[routing]\nenabled = true\nstray3 = 3\n";
        let (with, keys, _) = parse_reporting::<RawToml>(raw).unwrap();
        let plain: RawToml = toml::from_str(raw).unwrap();
        assert_eq!(keys.len(), 3);
        assert_eq!(format!("{with:?}"), format!("{plain:?}"));
    }
}
