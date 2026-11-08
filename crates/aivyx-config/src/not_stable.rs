//! The snapshot entries that are **not** part of pa's 1.x stable surface.
//!
//! `docs/stable/{config,cli,formats}.txt` list everything the code accepts
//! or writes, so a change to any of it is deliberate. Some of those lines
//! are not promised (docs/COMPATIBILITY.md, "Not stable"): the stable-surface
//! tests (`stable_config_keys` in this crate, `stable_` in aivyx-cli) end
//! each such line with its tag, e.g. `matrix.homeserver: str? (preview)`.
//! [`NOT_STABLE`] is the one list of them, matched by line prefix; both
//! tests fail when an entry matches no line, so a stale entry cannot stay.

/// Why a snapshot line is outside the promise. Each is named in
/// docs/COMPATIBILITY.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// Learning and recall tuning: read, but changes as the learning
    /// system does. A section's `enabled` switch stays stable.
    Advanced,
    /// Needs a non-default Cargo feature that no release binary has.
    BuildTimeFeature,
    /// A preview channel; the tag comes off when it passes its activation.
    Preview,
    /// A developer or diagnostic command.
    Developer,
    /// A versioned format that is recorded but not promised.
    NotStable,
}

impl Tag {
    /// The suffix the snapshot puts on a tagged line.
    pub const fn suffix(self) -> &'static str {
        match self {
            Tag::Advanced => " (advanced)",
            Tag::BuildTimeFeature => " (build-time feature)",
            Tag::Preview => " (preview)",
            Tag::Developer => " (developer)",
            Tag::NotStable => " (not stable)",
        }
    }
}

/// Which snapshot file an entry's prefix is matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snapshot {
    /// `config.txt`: the key path (`matrix.homeserver`), or
    /// `key = "value"` for one value of an enum key.
    Config,
    /// `cli.txt`: a usage line (`aivyx-pa matrix verify`), a global flag
    /// (`--print-role`), or `--flag value` for one value of a flag.
    Cli,
    /// `formats.txt`: the line itself (`tool.protocol_version`).
    Formats,
}

/// One not-stable entry: every line of `snapshot` that starts with
/// `prefix` gets `tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotStable {
    pub snapshot: Snapshot,
    pub prefix: &'static str,
    pub tag: Tag,
}

const fn entry(snapshot: Snapshot, prefix: &'static str, tag: Tag) -> NotStable {
    NotStable {
        snapshot,
        prefix,
        tag,
    }
}

use Snapshot::Cli;
use Snapshot::Config as C;
use Tag::Advanced as ADV;
use Tag::BuildTimeFeature as BUILD;
use Tag::Preview as PREVIEW;

/// Every not-stable snapshot entry (see the module docs).
pub const NOT_STABLE: &[NotStable] = &[
    // R6: learning and recall tuning. `memory.profile` (lite/smart) is the
    // stable knob; every `.enabled` switch under these stays stable too.
    entry(C, "correction_consolidation.", ADV),
    entry(C, "correction_judgment.", ADV),
    entry(C, "correction_signal.", ADV),
    entry(C, "embedding.ann_", ADV),
    entry(C, "embedding.rag_", ADV),
    entry(C, "embedding.recall_", ADV),
    entry(C, "graph.", ADV),
    entry(C, "ollama.prompt_strategies", ADV),
    entry(C, "persona.auto_propose.", ADV),
    entry(C, "persona_consolidation.", ADV),
    entry(C, "persona_lifecycle.", ADV),
    entry(C, "providers.", ADV),
    entry(C, "recall_cluster.", ADV),
    entry(C, "recall_feedback.", ADV),
    entry(C, "recall_judgment.", ADV),
    entry(C, "skill_authoring.", ADV),
    entry(C, "skill_refinement.", ADV),
    entry(C, "skills.auto_propose.", ADV),
    entry(C, "tool_relevance.", ADV),
    entry(C, "wiki.", ADV),
    // R9: features no release binary has (`provider-mistral-rs`,
    // `channel-voice`). `federation yubikey-init` (`yubikey`) is left out of
    // cli.txt instead: help shows it only in a build with the feature.
    entry(C, "agent.provider = \"mistral", BUILD),
    entry(C, "mistralrs.", BUILD),
    entry(C, "voice.", BUILD),
    entry(Cli, "--channel voice", BUILD),
    entry(Cli, "--provider mistralrs", BUILD),
    // R11: the preview channels. A channel's entries come off when it passes
    // its activation session with a real client (Launch chapter, Tasks
    // 14-16: Matrix, Proton Mail, Signal).
    entry(C, "matrix.", PREVIEW),
    entry(C, "proton.", PREVIEW),
    entry(C, "signal.", PREVIEW),
    entry(Cli, "aivyx-pa matrix verify", PREVIEW),
    entry(Cli, "aivyx-pa signal verify", PREVIEW),
    entry(Cli, "--channel matrix", PREVIEW),
    entry(Cli, "--channel proton", PREVIEW),
    entry(Cli, "--channel signal", PREVIEW),
    // R26: the help's Developer group, and a debugging flag.
    // `--verify-only` (forensics) stays stable.
    entry(Cli, "aivyx-pa tool-relevance", Tag::Developer),
    entry(Cli, "--print-role", Tag::Developer),
    // R1 (b): format 1 pack bundles are stable, the tool protocol their tools
    // speak is not (yet): a 1.x minor stabilises it once a real tool pack
    // exists.
    entry(Snapshot::Formats, "tool.protocol_version", Tag::NotStable),
];

/// The tag of the snapshot line whose key (see [`Snapshot`]) is `key`.
pub fn tag_for(snapshot: Snapshot, key: &str) -> Option<Tag> {
    NOT_STABLE
        .iter()
        .filter(|e| e.snapshot == snapshot && key.starts_with(e.prefix))
        .find(|e| !(e.tag == Tag::Advanced && key.ends_with(".enabled")))
        .map(|e| e.tag)
}

/// Entries of `snapshot` that tag no line of `lines` (each line already
/// tagged): stale, so the tests fail on them.
pub fn stale_entries<'a>(
    snapshot: Snapshot,
    lines: impl Iterator<Item = &'a str> + Clone,
) -> Vec<&'static str> {
    NOT_STABLE
        .iter()
        .filter(|e| e.snapshot == snapshot)
        .filter(|e| {
            !lines
                .clone()
                .any(|l| l.trim_start().starts_with(e.prefix) && l.ends_with(e.tag.suffix()))
        })
        .map(|e| e.prefix)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_advanced_sections_enabled_switch_stays_stable() {
        assert_eq!(tag_for(C, "graph.interval_secs"), Some(Tag::Advanced));
        assert_eq!(tag_for(C, "graph.enabled"), None);
        assert_eq!(
            tag_for(C, "persona.auto_propose.assistant_name.enabled"),
            None
        );
        assert_eq!(tag_for(C, "memory.profile"), None);
        assert_eq!(tag_for(Snapshot::Cli, "graph.interval_secs"), None);
    }

    #[test]
    fn a_prefix_matching_no_line_is_stale() {
        let lines = ["graph.interval_secs: u64? (advanced)"];
        let stale = stale_entries(C, lines.iter().copied());
        assert!(!stale.contains(&"graph."), "{stale:?}");
        assert!(stale.contains(&"wiki."), "{stale:?}");
    }
}
