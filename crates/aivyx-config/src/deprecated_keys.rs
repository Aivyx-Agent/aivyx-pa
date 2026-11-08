//! Deprecated `config.toml` spellings (1.0 readiness audit, R7 and on).
//!
//! A renamed key keeps working through 1.x: before the typed parse, the
//! parsed TOML table is rewritten so the old spelling's value sits at the new
//! path, and one warning per old spelling goes to `AivyxConfig.warnings`
//! (the list the unknown-key warnings use). An old spelling is therefore
//! never also reported as an unknown key. When both spellings are set, the
//! new one wins and the warning says so.
//!
//! A few old keys are not renames: they are still read where they are (and
//! keep their own precedence), and only warn. Those are [`Change::Deprecated`].
//!
//! The list is part of pa's stable surface: `stable_config_keys` prints each
//! rename as `old: deprecated alias of new` and marks each deprecated key's
//! own line with `(deprecated)` in `docs/stable/config.txt`.

/// What became of one old spelling.
pub(crate) enum Change {
    /// Read at this new path instead; the new path wins when both are set.
    Renamed(&'static str),
    /// Still read where it is; the text says what to use instead.
    Deprecated(&'static str),
}

/// Every deprecated spelling, as a dotted path. A `[]` segment is an array
/// of tables (`schedule[].notify_target`: the key in any `[[schedule]]`);
/// renames use plain `section.key` paths only.
pub(crate) const DEPRECATED_KEYS: &[(&str, Change)] = &[
    // R7: the aivyx-confine settings live under `[sandbox]`, as in coder.
    ("confine.require_enforcement", Change::Renamed("sandbox.require_enforcement")),
    ("confine.allow_unix_sockets", Change::Renamed("sandbox.allow_unix_sockets")),
    (
        "confine.allow_leaving_process_group",
        Change::Renamed("sandbox.allow_leaving_process_group"),
    ),
    ("confine.share_system_tmp", Change::Renamed("sandbox.share_system_tmp")),
    // R17: the singular is still read (setting both stays an error).
    (
        "schedule[].notify_target",
        Change::Deprecated("use `notify_targets = [\"<name>\"]`"),
    ),
    (
        "webhook[].notify_target",
        Change::Deprecated("use `notify_targets = [\"<name>\"]`"),
    ),
    (
        "file_watch[].notify_target",
        Change::Deprecated("use `notify_targets = [\"<name>\"]`"),
    ),
    // R17: Telegram's key has been `chat_id` since 2026-09-16.
    ("telegram.chat_filter", Change::Renamed("telegram.chat_id")),
];

fn segments(path: &str) -> impl Iterator<Item = (&str, bool)> + Clone {
    path.split('.').map(|s| match s.strip_suffix("[]") {
        Some(name) => (name, true),
        None => (s, false),
    })
}

/// Whether `path` is set anywhere (any element of a `[]` array counts).
fn is_set(table: &toml::Table, path: &str) -> bool {
    fn walk<'a>(t: &toml::Table, mut segs: impl Iterator<Item = (&'a str, bool)> + Clone) -> bool {
        let Some((name, is_array)) = segs.next() else { return true };
        match (t.get(name), is_array) {
            (Some(toml::Value::Table(inner)), false) => walk(inner, segs),
            (Some(toml::Value::Array(items)), true) => items.iter().any(|item| match item {
                toml::Value::Table(inner) => walk(inner, segs.clone()),
                _ => false,
            }),
            (Some(_), false) => segs.next().is_none(),
            _ => false,
        }
    }
    walk(table, segments(path))
}

/// The table holding `path`'s last segment, created on the way when `create`.
fn parent_mut<'a>(table: &'a mut toml::Table, path: &str, create: bool) -> Option<&'a mut toml::Table> {
    let (parent, _) = path.rsplit_once('.')?;
    let mut cur = table;
    for seg in parent.split('.') {
        if create && !cur.contains_key(seg) {
            cur.insert(seg.to_string(), toml::Value::Table(toml::Table::new()));
        }
        cur = match cur.get_mut(seg)? {
            toml::Value::Table(t) => t,
            _ => return None,
        };
    }
    Some(cur)
}

fn leaf(path: &str) -> &str {
    path.rsplit_once('.').map_or(path, |(_, l)| l)
}

/// Moves every renamed key to its new path and returns one warning per
/// deprecated spelling found, in [`DEPRECATED_KEYS`] order.
pub(crate) fn rewrite(table: &mut toml::Table) -> Vec<String> {
    let mut warnings = Vec::new();
    for (old, change) in DEPRECATED_KEYS {
        if !is_set(table, old) {
            continue;
        }
        match change {
            Change::Deprecated(instead) => {
                warnings.push(format!("`{old}` is deprecated; {instead}"));
            }
            Change::Renamed(new) => {
                if is_set(table, new) {
                    // Both set: the new spelling wins; the old one is dropped.
                    if let Some(parent) = parent_mut(table, old, false) {
                        parent.remove(leaf(old));
                    }
                    warnings.push(format!(
                        "`{old}` is deprecated; use `{new}` (both are set: `{new}` wins, \
                         `{old}` is ignored)"
                    ));
                } else {
                    let value = parent_mut(table, old, false).and_then(|p| p.remove(leaf(old)));
                    if let Some(value) = value {
                        match parent_mut(table, new, true) {
                            Some(parent) => {
                                parent.insert(leaf(new).to_string(), value);
                            }
                            // The new section is not a table (`studio = 1`):
                            // leave the old key where it was; the typed parse
                            // reports the bad section.
                            None => {
                                if let Some(p) = parent_mut(table, old, false) {
                                    p.insert(leaf(old).to_string(), value);
                                }
                            }
                        }
                    }
                    warnings.push(format!("`{old}` is deprecated; use `{new}`"));
                }
                // A section left empty by the move (`[confine]`) goes too, so
                // it is not reported as an unknown section.
                if let Some((section, _)) = old.split_once('.') {
                    if matches!(table.get(section), Some(toml::Value::Table(t)) if t.is_empty()) {
                        table.remove(section);
                    }
                }
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(raw: &str) -> (toml::Table, Vec<String>) {
        let mut t: toml::Table = raw.parse().unwrap();
        let w = rewrite(&mut t);
        (t, w)
    }

    #[test]
    fn a_renamed_key_moves_and_warns() {
        let (t, w) = rewritten("[confine]\nshare_system_tmp = true\n");
        assert_eq!(t["sandbox"]["share_system_tmp"].as_bool(), Some(true));
        assert!(!t.contains_key("confine"), "{t:?}");
        assert_eq!(
            w,
            vec!["`confine.share_system_tmp` is deprecated; use `sandbox.share_system_tmp`"]
        );
    }

    #[test]
    fn the_new_spelling_wins_when_both_are_set() {
        let (t, w) = rewritten(
            "[confine]\nrequire_enforcement = false\n[sandbox]\nrequire_enforcement = true\n",
        );
        assert_eq!(t["sandbox"]["require_enforcement"].as_bool(), Some(true));
        assert!(!t.contains_key("confine"));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("both are set: `sandbox.require_enforcement` wins"), "{w:?}");
    }

    #[test]
    fn other_keys_in_the_old_section_stay_for_the_unknown_key_report() {
        let (t, _) = rewritten("[confine]\nshare_system_tmp = true\nbogus = 1\n");
        assert_eq!(t["confine"]["bogus"].as_integer(), Some(1));
    }

    #[test]
    fn nothing_deprecated_changes_nothing() {
        let raw = "[sandbox]\ndefault_backend = \"bwrap\"\n";
        let (t, w) = rewritten(raw);
        assert!(w.is_empty());
        assert_eq!(t, raw.parse::<toml::Table>().unwrap());
    }

    #[test]
    fn is_set_looks_into_arrays_of_tables() {
        let t: toml::Table = "[[schedule]]\nname = \"a\"\n[[schedule]]\nnotify_target = \"x\"\n"
            .parse()
            .unwrap();
        assert!(is_set(&t, "schedule[].notify_target"));
        assert!(!is_set(&t, "schedule[].cron"));
    }
}
