//! Which sidebar groups are open, remembered per browser in `localStorage`.
//!
//! Storage can be missing or throw (a private window, blocked site data);
//! every failure is ignored and the sidebar just starts with its groups
//! closed.

use std::collections::BTreeSet;

/// Missions → Mission Control.
pub const MISSIONS: &str = "missions";
/// Memory → Wiki, Graph.
pub const MEMORY: &str = "memory";
/// The collapsed "More" group: Gallery, Voice, Sessions, MCP, Tools.
pub const MORE: &str = "more";

const STORAGE_KEY: &str = "aivyx.studio.nav-open";

/// The open groups, as stored.
pub type OpenGroups = BTreeSet<String>;

/// The group a nested screen (by its URL slug) lives in. Top-level screens
/// have none.
pub fn group_of(slug: &str) -> Option<&'static str> {
    match slug {
        "mission-control" => Some(MISSIONS),
        "wiki" | "graph" => Some(MEMORY),
        "gallery" | "voice" | "sessions" | "mcp" | "tools" => Some(MORE),
        _ => None,
    }
}

/// Parses the stored value: known group names separated by commas;
/// anything else is dropped.
pub fn parse(stored: &str) -> OpenGroups {
    stored
        .split(',')
        .map(str::trim)
        .filter(|g| [MISSIONS, MEMORY, MORE].contains(g))
        .map(str::to_string)
        .collect()
}

/// The value to store.
pub fn serialize(open: &OpenGroups) -> String {
    open.iter().cloned().collect::<Vec<_>>().join(",")
}

/// The open groups saved in this browser (none if storage is unavailable).
pub fn load() -> OpenGroups {
    storage()
        .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten())
        .map(|v| parse(&v))
        .unwrap_or_default()
}

/// Saves the open groups; a failure is ignored.
pub fn save(open: &OpenGroups) {
    if let Some(s) = storage() {
        let _ = s.set_item(STORAGE_KEY, &serialize(open));
    }
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_screens_belong_to_their_group() {
        assert_eq!(group_of("mission-control"), Some(MISSIONS));
        assert_eq!(group_of("graph"), Some(MEMORY));
        assert_eq!(group_of("sessions"), Some(MORE));
        assert_eq!(group_of("tools"), Some(MORE));
        // Pinned and top-level screens are never hidden.
        assert_eq!(group_of("guide"), None);
        assert_eq!(group_of("settings"), None);
        assert_eq!(group_of("memory"), None);
    }

    #[test]
    fn the_stored_value_round_trips_and_ignores_junk() {
        let open = parse("more, memory,bogus,,");
        assert_eq!(serialize(&open), "memory,more");
        assert_eq!(parse(&serialize(&open)), open);
        assert!(parse("").is_empty());
    }
}
