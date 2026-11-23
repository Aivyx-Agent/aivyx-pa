//! The team-config schema — the declarative `[team]` TOML a vertical pack
//! (or the operator) supplies. A team is a `lead` plus members; each
//! member is a **persona + scoped role** (`soul` / `tool_allowlist` /
//! `capability_scopes` / `trust_ceiling`), reusing the new core's Role
//! shape.

use std::collections::HashSet;
use std::path::Path;

use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use serde::{Deserialize, Serialize};

/// The Nonagon bound: 9 members TOTAL — a lead plus at most 8
/// specialists (nine-sided figure, nine seats). Was originally
/// specialists-only (lead + 9 = 10 total), letting a 10th member
/// break the shape the name promises; corrected alongside Chapter
/// Nonagon Templates (2026-07-06, operator-confirmed) once the
/// Studio's own "Max 9 specialists" button made the mismatch visible.
pub const MAX_SPECIALISTS: usize = 8;
/// Total members including the lead — the number the "Nonagon" name
/// actually refers to.
pub const MAX_TOTAL_MEMBERS: usize = MAX_SPECIALISTS + 1;

/// Errors loading or validating a team config.
#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("team config: {0}")]
    Config(String),
    #[error("invalid capability scope {0:?}")]
    Scope(String),
    #[error("TOML: {0}")]
    Toml(String),
    #[error("io: {0}")]
    Io(String),
    /// A member's `base_url` is not on this machine or the operator's
    /// network. `reason` is the local-only guard's `NotLocal` text.
    #[error("team member `{member}`: {reason}")]
    NotLocal { member: String, reason: String },
}

/// One team member — a persona + a scoped role. Maps onto the new core's
/// Role shape; the actual specialist agent is constructed (attenuated) by
/// the `SpecialistPool` in J.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    /// Member id (unique within the team; `a-z A-Z 0-9 _ -`).
    pub name: String,
    /// Human-readable team role, e.g. "Food-Safety / Compliance".
    pub role: String,
    /// The member's system prompt.
    pub soul: String,
    /// Tools this member may call (names; filtered at registration in J.2+).
    #[serde(default)]
    pub tool_allowlist: Vec<String>,
    /// Capability scopes the member *declares*. Attenuated against the
    /// lead at spawn time (NT-02) — declaring a scope the lead lacks buys
    /// nothing.
    #[serde(default)]
    pub capability_scopes: Vec<String>,
    /// The member's declared trust ceiling, floored to the lead's at spawn.
    pub trust_ceiling: TrustTier,
    /// Chapter Ensemble — optional per-role model override. `None` ⇒ the
    /// team's shared default model. Lets a coordinator run on a big model
    /// while grunt specialists run on a small fast one (role-fit + cost).
    #[serde(default)]
    pub model: Option<String>,
    /// Chapter Ensemble — optional per-role endpoint (same provider kind as
    /// the daemon's). `None` ⇒ the shared default endpoint. Pointing roles at
    /// different endpoints (e.g. a second Ollama / GPU) gives TRUE parallel
    /// execution instead of serializing on one server. Cross-provider-kind
    /// (e.g. a cloud lead + local grunts) is a deliberate later step.
    #[serde(default)]
    pub base_url: Option<String>,
}

impl TeamMember {
    /// Parse the declared scope strings, erroring on any unknown base.
    pub fn parsed_scopes(&self) -> Result<Vec<Scope>, TeamError> {
        self.capability_scopes
            .iter()
            .map(|s| Scope::parse(s).ok_or_else(|| TeamError::Scope(s.clone())))
            .collect()
    }

    /// The member's declared capabilities as a `CapabilitySet` (before
    /// attenuation against a lead).
    pub fn declared_capabilities(&self) -> Result<CapabilitySet, TeamError> {
        Ok(CapabilitySet::from_scopes(self.parsed_scopes()?))
    }
}

/// Inter-specialist dialogue + spawn limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueConfig {
    #[serde(default = "default_enable_peer_dialogue")]
    pub enable_peer_dialogue: bool,
    #[serde(default = "default_max_messages_per_turn")]
    pub max_messages_per_turn: u32,
    #[serde(default = "default_max_spawned_specialists")]
    pub max_spawned_specialists: usize,
    #[serde(default = "default_delegation_timeout_secs")]
    pub delegation_timeout_secs: u64,
    #[serde(default = "default_message_bus_capacity")]
    pub message_bus_capacity: usize,
}

fn default_enable_peer_dialogue() -> bool {
    true
}
fn default_max_messages_per_turn() -> u32 {
    10
}
fn default_max_spawned_specialists() -> usize {
    5
}
fn default_delegation_timeout_secs() -> u64 {
    600
}
fn default_message_bus_capacity() -> usize {
    64
}

impl Default for DialogueConfig {
    fn default() -> Self {
        DialogueConfig {
            enable_peer_dialogue: default_enable_peer_dialogue(),
            max_messages_per_turn: default_max_messages_per_turn(),
            max_spawned_specialists: default_max_spawned_specialists(),
            delegation_timeout_secs: default_delegation_timeout_secs(),
            message_bus_capacity: default_message_bus_capacity(),
        }
    }
}

/// A team: a lead that coordinates up to 9 attenuated specialists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The lead member's `name` — must be one of `members`.
    pub lead: String,
    #[serde(default, rename = "member")]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub dialogue: DialogueConfig,
}

// The `[team]`-rooted TOML wrapper (see docs/NONAGON.md §3).
#[derive(Deserialize)]
struct TeamFile {
    team: TeamConfig,
}
#[derive(Serialize)]
struct TeamFileRef<'a> {
    team: &'a TeamConfig,
}

impl TeamConfig {
    /// Parse + validate a `[team]`-rooted TOML document.
    pub fn from_toml(s: &str) -> Result<Self, TeamError> {
        let file: TeamFile = toml::from_str(s).map_err(|e| TeamError::Toml(e.to_string()))?;
        file.team.validate()?;
        Ok(file.team)
    }

    /// [`Self::from_toml`], then run `check` on every member's `base_url`
    /// (members without one are skipped). A refusal becomes
    /// [`TeamError::NotLocal`] naming the member. The check is injected so
    /// this crate stays dependency-light (wasm-clean); aivyx-team passes
    /// the local-only guard.
    pub fn from_toml_checked(
        s: &str,
        check: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Self, TeamError> {
        let team = Self::from_toml(s)?;
        team.check_base_urls(check)?;
        Ok(team)
    }

    /// [`Self::load`] with the same per-member `base_url` check as
    /// [`Self::from_toml_checked`].
    pub fn load_checked(
        path: impl AsRef<Path>,
        check: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Self, TeamError> {
        let team = Self::load(path)?;
        team.check_base_urls(check)?;
        Ok(team)
    }

    /// Run `check` on every member `base_url`.
    pub fn check_base_urls(&self, check: &dyn Fn(&str) -> Result<(), String>) -> Result<(), TeamError> {
        for m in &self.members {
            if let Some(url) = m.base_url.as_deref() {
                check(url).map_err(|reason| TeamError::NotLocal {
                    member: m.name.clone(),
                    reason,
                })?;
            }
        }
        Ok(())
    }

    /// Serialize back to `[team]`-rooted TOML.
    pub fn to_toml(&self) -> Result<String, TeamError> {
        toml::to_string_pretty(&TeamFileRef { team: self }).map_err(|e| TeamError::Toml(e.to_string()))
    }

    /// Load + validate from a file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, TeamError> {
        let s = std::fs::read_to_string(path).map_err(|e| TeamError::Io(e.to_string()))?;
        Self::from_toml(&s)
    }

    /// The lead member, if present.
    pub fn lead_member(&self) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.name == self.lead)
    }

    /// Every member that is not the lead (the specialists).
    pub fn specialists(&self) -> impl Iterator<Item = &TeamMember> {
        self.members.iter().filter(move |m| m.name != self.lead)
    }

    /// Validate names, uniqueness, scopes, the lead-is-a-member rule, and
    /// the ≤9-specialist Nonagon bound.
    pub fn validate(&self) -> Result<(), TeamError> {
        validate_name(&self.name, "team")?;
        if self.members.is_empty() {
            return Err(TeamError::Config("team has no members".into()));
        }

        let mut seen = HashSet::new();
        for m in &self.members {
            validate_name(&m.name, "member")?;
            if !seen.insert(m.name.as_str()) {
                return Err(TeamError::Config(format!("duplicate member name {:?}", m.name)));
            }
            m.parsed_scopes()?; // every declared scope must parse (known base)
        }

        if self.lead_member().is_none() {
            return Err(TeamError::Config(format!(
                "lead {:?} is not listed in team members",
                self.lead
            )));
        }

        let specialist_count = self.members.len() - 1; // lead is one member
        if specialist_count > MAX_SPECIALISTS {
            return Err(TeamError::Config(format!(
                "a Nonagon allows at most {MAX_SPECIALISTS} specialists (9 total \
                 with the lead), got {specialist_count} specialists \
                 ({} total)",
                self.members.len()
            )));
        }

        if self.dialogue.message_bus_capacity == 0 {
            return Err(TeamError::Config("message_bus_capacity must be at least 1".into()));
        }
        Ok(())
    }
}

/// Names key the session store and appear in logs — keep them safe.
fn validate_name(name: &str, label: &str) -> Result<(), TeamError> {
    if name.is_empty() || name.len() > 128 {
        return Err(TeamError::Config(format!(
            "{label}: name must be 1-128 characters, got {}",
            name.len()
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(TeamError::Config(format!(
            "{label}: name {name:?} has invalid characters (a-z A-Z 0-9 _ - only)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, scopes: &[&str], tier: TrustTier) -> TeamMember {
        TeamMember {
            name: name.into(),
            role: "Role".into(),
            soul: "soul".into(),
            tool_allowlist: vec![],
            capability_scopes: scopes.iter().map(|s| s.to_string()).collect(),
            trust_ceiling: tier,
            model: None,
            base_url: None,
        }
    }

    #[test]
    fn ensemble_member_model_and_base_url_round_trip() {
        // Chapter Ensemble — per-role model/base_url parse + survive a round-trip;
        // absent → None.
        let toml = "\n[team]\nname = \"t\"\nlead = \"lead\"\n\
            \n[[team.member]]\nname = \"lead\"\nrole = \"R\"\nsoul = \"s\"\n\
            trust_ceiling = \"Trusted\"\nmodel = \"qwen3:30b\"\n\
            \n[[team.member]]\nname = \"grunt\"\nrole = \"R\"\nsoul = \"s\"\n\
            trust_ceiling = \"Trusted\"\nmodel = \"qwen3:8b\"\nbase_url = \"http://gpu-b:11434\"\n";
        let cfg = TeamConfig::from_toml(toml).unwrap();
        let lead = cfg.members.iter().find(|m| m.name == "lead").unwrap();
        assert_eq!(lead.model.as_deref(), Some("qwen3:30b"));
        assert_eq!(lead.base_url, None);
        let grunt = cfg.members.iter().find(|m| m.name == "grunt").unwrap();
        assert_eq!(grunt.model.as_deref(), Some("qwen3:8b"));
        assert_eq!(grunt.base_url.as_deref(), Some("http://gpu-b:11434"));
        // Round-trips.
        assert_eq!(TeamConfig::from_toml(&cfg.to_toml().unwrap()).unwrap(), cfg);
    }

    fn team(members: Vec<TeamMember>, lead: &str) -> TeamConfig {
        TeamConfig {
            name: "t".into(),
            description: String::new(),
            lead: lead.into(),
            members,
            dialogue: DialogueConfig::default(),
        }
    }

    #[test]
    fn toml_roundtrip_through_the_team_wrapper() {
        let cfg = team(
            vec![
                member("aria", &["kitchen.read"], TrustTier::Trusted),
                member("haccp", &["kitchen.haccp.log"], TrustTier::Trusted),
            ],
            "aria",
        );
        let toml = cfg.to_toml().unwrap();
        assert!(toml.contains("[team]"));
        let back = TeamConfig::from_toml(&toml).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.lead_member().unwrap().name, "aria");
        assert_eq!(back.specialists().count(), 1);
    }

    #[test]
    fn dialogue_defaults_apply_when_omitted() {
        let toml = r#"
[team]
name = "t"
lead = "lead"
[[team.member]]
name = "lead"
role = "Lead"
soul = "s"
trust_ceiling = "Trusted"
"#;
        let cfg = TeamConfig::from_toml(toml).unwrap();
        assert!(cfg.dialogue.enable_peer_dialogue);
        assert_eq!(cfg.dialogue.max_spawned_specialists, 5);
        assert_eq!(cfg.dialogue.message_bus_capacity, 64);
    }

    #[test]
    fn lead_must_be_a_member() {
        let err = team(vec![member("a", &[], TrustTier::Trusted)], "ghost")
            .validate()
            .unwrap_err();
        assert!(matches!(err, TeamError::Config(m) if m.contains("lead")));
    }

    #[test]
    fn rejects_more_than_eight_specialists() {
        // 9 specialists + lead = 10 total, one over the Nonagon's 9 seats.
        let mut members = vec![member("lead", &[], TrustTier::Trusted)];
        for i in 0..9 {
            members.push(member(&format!("s{i}"), &[], TrustTier::Trusted));
        }
        let err = team(members, "lead").validate().unwrap_err();
        assert!(matches!(err, TeamError::Config(m) if m.contains("at most 8")));
    }

    #[test]
    fn exactly_eight_specialists_is_nine_total_and_allowed() {
        let mut members = vec![member("lead", &[], TrustTier::Trusted)];
        for i in 0..8 {
            members.push(member(&format!("s{i}"), &[], TrustTier::Trusted));
        }
        assert_eq!(members.len(), MAX_TOTAL_MEMBERS);
        assert!(team(members, "lead").validate().is_ok());
    }

    #[test]
    fn rejects_duplicate_and_unsafe_names() {
        let dup = team(
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("lead", &[], TrustTier::Trusted),
            ],
            "lead",
        );
        assert!(matches!(dup.validate(), Err(TeamError::Config(m)) if m.contains("duplicate")));

        let bad = team(vec![member("bad name!", &[], TrustTier::Trusted)], "bad name!");
        assert!(matches!(bad.validate(), Err(TeamError::Config(m)) if m.contains("invalid characters")));
    }

    #[test]
    fn rejects_unknown_capability_scope() {
        let cfg = team(
            vec![member("lead", &["not.a.real.base"], TrustTier::Trusted)],
            "lead",
        );
        assert!(matches!(cfg.validate(), Err(TeamError::Scope(s)) if s == "not.a.real.base"));
    }

    #[test]
    fn rejects_zero_message_bus_capacity() {
        let mut t = team(vec![member("lead", &[], TrustTier::Trusted)], "lead");
        t.dialogue.message_bus_capacity = 0;
        assert!(matches!(t.validate(), Err(TeamError::Config(m)) if m.contains("message_bus_capacity")));
    }

    #[test]
    fn rejects_empty_team() {
        let t = team(vec![], "lead");
        assert!(matches!(t.validate(), Err(TeamError::Config(m)) if m.contains("no members")));
    }

    #[test]
    fn malformed_toml_errors() {
        assert!(matches!(
            TeamConfig::from_toml("this is = not [ valid"),
            Err(TeamError::Toml(_))
        ));
    }

    #[test]
    fn member_declared_capabilities_parse() {
        let m = member("haccp", &["kitchen.haccp.log"], TrustTier::Trusted);
        let caps = m.declared_capabilities().unwrap();
        assert!(caps.grants(&Scope::parse("kitchen.haccp.log").unwrap()));
    }
}
