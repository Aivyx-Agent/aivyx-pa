//! Chapter — real `--help` / `-h` / `help` for `aivyx-pa` and every
//! subcommand. Spec:
//! `docs/superpowers/specs/2026-09-29-first-run-coherence-design.md` §A3.
//!
//! Before this module, `aivyx-pa --help`, `-h` and `help` all fell through
//! to `parse_cli_args_from`'s final "unrecognized argument" arm — the
//! 2026-09-28 UI/UX audit flagged this as the very first thing a new
//! operator hits. [`COMMANDS`] is the single source of truth for every
//! top-level subcommand's one-line summary and usage; [`intercept`] is
//! called at the top of `parse_cli_args_from` (so no subcommand module's
//! own parsing has to change), and `aivyx.rs`'s
//! `help_table_has_no_drift_from_dispatcher` test scans the parser source
//! for every `args[0] == "..."` dispatch and asserts each has an entry
//! here — add a subcommand without a table row, and that test fails.
//!
//! Never invent a flag here: every usage line below was read off the
//! subcommand's own parsing in `aivyx.rs` (or, for `--verify-only` /
//! `--channel` / `--role` / `--print-role` / `--no-daemon` / `--provider`
//! / `--mcp-server` / `--mcp-sse`, the REPL flag loop at the end of
//! `parse_cli_args_from`).

/// Which grouped section of the top-level help a command's entry prints
/// under. Declaration order of [`COMMANDS`] within a group is the order
/// it prints in; group order here matches spec A3's listed grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    FrontEnds,
    Daemon,
    Setup,
    Agent,
    Tools,
    Inspection,
}

/// One top-level subcommand's help-table entry.
pub struct CommandHelp {
    /// Exactly the string matched by `args[0] == "<name>"` in
    /// `parse_cli_args_from` — this is also what `<name> --help`/`-h`
    /// matches against in [`intercept`].
    pub name: &'static str,
    pub group: Group,
    /// One line, no trailing period, fits after a padded name column.
    pub summary: &'static str,
    /// One `aivyx-pa ...` line per accepted form. A line that doesn't
    /// start with `aivyx-pa` is a wrapped continuation of the line
    /// above it (kept as a separate entry only to stay under ~80
    /// columns).
    pub usage: &'static [&'static str],
}

/// `args[0]` values the dispatcher matches with `args[0] == "..."` that
/// are *not* real subcommands — the version probe short-circuits before
/// any other parsing and has no per-command help entry of its own (it's
/// listed under "Global flags" in [`render_top_level_help`] instead).
/// The drift-guard test excludes these from the "every dispatched name
/// needs a table entry" check. Only that test (in `aivyx.rs`'s
/// `mod tests`) reads this, so it's `cfg(test)` — otherwise the plain
/// (non-test) `bin` target in `--all-targets` sees it as dead code.
#[cfg(test)]
pub const NON_SUBCOMMAND_DISPATCH_NAMES: &[&str] = &["--version", "-V"];

pub const COMMANDS: &[CommandHelp] = &[
    // ---- Chat and front ends ----------------------------------------
    CommandHelp {
        name: "tui",
        group: Group::FrontEnds,
        summary: "Terminal UI (ratatui) session",
        usage: &["aivyx-pa tui [--role <name>]"],
    },
    CommandHelp {
        name: "--headless",
        group: Group::FrontEnds,
        summary: "One-shot or piped headless turn over the daemon",
        usage: &[
            "aivyx-pa --headless \"<task>\"",
            "aivyx-pa --headless          (stdin session mode, piped input)",
        ],
    },
    // ---- Daemon -------------------------------------------------------
    CommandHelp {
        name: "instances",
        group: Group::Setup,
        summary: "List, create, or remove named instances (separate agents)",
        usage: &[
            "aivyx-pa instances list",
            "aivyx-pa instances create <name>",
            "aivyx-pa instances remove <name>",
        ],
    },
    CommandHelp {
        name: "daemon",
        group: Group::Daemon,
        summary: "Run, check, stop, install, or uninstall the daemon",
        usage: &[
            "aivyx-pa daemon run [--web-ui] [--web-ui-port <N>]",
            "aivyx-pa daemon status",
            "aivyx-pa daemon stop",
            "aivyx-pa daemon install [--web-ui] [--no-start]",
            "aivyx-pa daemon uninstall",
        ],
    },
    // ---- Setup ----------------------------------------------------------
    CommandHelp {
        name: "init",
        group: Group::Setup,
        summary: "Interactive first-run setup wizard",
        usage: &["aivyx-pa init [--template <name>] [--list-templates]"],
    },
    CommandHelp {
        name: "doctor",
        group: Group::Setup,
        summary: "Diagnose config, storage, and provider connectivity",
        usage: &["aivyx-pa doctor"],
    },
    CommandHelp {
        name: "studio",
        group: Group::Setup,
        summary: "Print the Studio sign-in link (or its token)",
        usage: &["aivyx-pa studio", "aivyx-pa studio --token"],
    },
    CommandHelp {
        name: "keyring",
        group: Group::Setup,
        summary: "Manage the OS-keyring-stored passphrase",
        usage: &["aivyx-pa keyring set|clear|status"],
    },
    CommandHelp {
        name: "access",
        group: Group::Setup,
        summary: "Show or set the filesystem/network access level",
        usage: &[
            "aivyx-pa access show",
            "aivyx-pa access set <sandbox|workspace|home|full|custom>",
            "    [--root <dir>] [--yes]",
        ],
    },
    CommandHelp {
        name: "autonomy",
        group: Group::Setup,
        summary: "Show or set the autonomy level",
        usage: &[
            "aivyx-pa autonomy show",
            "aivyx-pa autonomy set <manual|assisted|supervised|autonomous|unleashed>",
            "    [--yes]",
        ],
    },
    // ---- The agent ------------------------------------------------------
    CommandHelp {
        name: "role",
        group: Group::Agent,
        summary: "Import an approved role-change proposal",
        usage: &["aivyx-pa role import <proposal-id> [--yes] [--force]"],
    },
    CommandHelp {
        name: "persona",
        group: Group::Agent,
        summary: "Inspect and manage the agent's learned persona",
        usage: &[
            "aivyx-pa persona show",
            "aivyx-pa persona list [--auto-only | --manual-only]",
            "aivyx-pa persona revert <delta-id>",
            "aivyx-pa persona proposals list",
            "    [--status <pending|approved|rejected|superseded|all>]",
            "aivyx-pa persona proposals show <proposal-id>",
            "aivyx-pa persona proposals approve <proposal-id>",
            "aivyx-pa persona proposals reject <proposal-id> [--reason <text>]",
            "aivyx-pa persona conflicts",
            "aivyx-pa persona resolve <id> --remove <a|b>",
            "aivyx-pa persona dismiss <id>",
        ],
    },
    CommandHelp {
        name: "profile",
        group: Group::Agent,
        summary: "Show, edit, or apply hints to the operator profile",
        usage: &[
            "aivyx-pa profile show",
            "aivyx-pa profile edit",
            "aivyx-pa profile apply-hint <proposal-id> [--yes]",
        ],
    },
    CommandHelp {
        name: "skills",
        group: Group::Agent,
        summary: "Teach, update, or forget a manual skill",
        usage: &[
            "aivyx-pa skills teach <name> <trigger> <procedure>",
            "aivyx-pa skills update <name> [--trigger <t>] [--procedure <p>]",
            "aivyx-pa skills forget <name>",
        ],
    },
    CommandHelp {
        name: "team",
        group: Group::Agent,
        summary: "Run and manage multi-agent (Nonagon) team missions",
        usage: &[
            "aivyx-pa team roster [--config <path>]",
            "aivyx-pa team init [--pack <default|path>] [--out <path>] [--force]",
            "aivyx-pa team run \"<mission>\" [--config <path>]",
            "aivyx-pa team start \"<goal>\" [--config <path>]",
            "aivyx-pa team start --plan <file.json> [--config <path>]",
            "aivyx-pa team list",
            "aivyx-pa team status [<mission-id>]",
            "aivyx-pa team approve|reject <mission-id> <step>",
            "aivyx-pa team abort|pause|resume <mission-id>",
        ],
    },
    CommandHelp {
        name: "loop",
        group: Group::Agent,
        summary: "Stock the autonomous backlog and drive runs",
        usage: &[
            "aivyx-pa loop add <title> [<body>] [--body <text>] [--priority <N>]",
            "aivyx-pa loop list",
            "aivyx-pa loop start [--max-iterations <N>]",
            "aivyx-pa loop status",
            "aivyx-pa loop stop",
            "aivyx-pa loop skip <story-id>",
            "aivyx-pa loop log [--limit <N>]",
        ],
    },
    CommandHelp {
        name: "review",
        group: Group::Agent,
        summary: "Review steps parked for your approval",
        usage: &[
            "aivyx-pa review [list]",
            "aivyx-pa review approve <id> [--yes]",
            "aivyx-pa review deny <id>",
        ],
    },
    CommandHelp {
        name: "memory",
        group: Group::Agent,
        summary: "Inspect, search, and manage agent memory",
        usage: &[
            "aivyx-pa memory list",
            "aivyx-pa memory show <topic> [--limit <N>]",
            "aivyx-pa memory search <query> [--semantic] [--limit <N>]",
            "aivyx-pa memory evict <topic> [--yes]",
            "aivyx-pa memory wiki [topic]",
            "aivyx-pa memory graph [entity]",
            "aivyx-pa memory conflicts",
            "aivyx-pa memory resolve <topic> --archive <seq>",
            "aivyx-pa memory dismiss <id>",
        ],
    },
    CommandHelp {
        name: "learning",
        group: Group::Agent,
        summary: "Show what the agent has learned recently",
        usage: &["aivyx-pa learning [--window <secs>]"],
    },
    // ---- Tools and integrations ------------------------------------------
    CommandHelp {
        name: "tools",
        group: Group::Tools,
        summary: "Show recent tool-call activity",
        usage: &["aivyx-pa tools [--window <secs>]"],
    },
    CommandHelp {
        name: "tool",
        group: Group::Tools,
        summary: "Scaffold a new out-of-process tool",
        usage: &["aivyx-pa tool init <path> [--force]"],
    },
    CommandHelp {
        name: "mcp",
        group: Group::Tools,
        summary: "Inspect MCP server status and recipes",
        usage: &["aivyx-pa mcp status", "aivyx-pa mcp recipes [name]"],
    },
    CommandHelp {
        name: "mcp-server",
        group: Group::Tools,
        summary: "Run a bundled MCP server",
        usage: &["aivyx-pa mcp-server web-search"],
    },
    CommandHelp {
        name: "connect",
        group: Group::Tools,
        summary: "Guided OAuth credential onboarding",
        usage: &["aivyx-pa connect", "aivyx-pa connect <service>"],
    },
    CommandHelp {
        name: "pack",
        group: Group::Tools,
        summary: "Build, inspect, and install signed vertical packs",
        usage: &[
            "aivyx-pa pack keygen <keyfile>",
            "aivyx-pa pack build <staging-dir> --key <keyfile> --out <file>",
            "aivyx-pa pack inspect <bundle-file> [--allow-untrusted]",
            "aivyx-pa pack install <bundle-file>",
        ],
    },
    CommandHelp {
        name: "notify",
        group: Group::Tools,
        summary: "Show notification delivery history",
        usage: &["aivyx-pa notify history [--target <name>] [--limit <N>]"],
    },
    // ---- Inspection -----------------------------------------------------
    CommandHelp {
        name: "audit",
        group: Group::Inspection,
        summary: "Export the HMAC-chained audit log",
        usage: &[
            "aivyx-pa audit export [--from <seq>] [--limit <N>]",
            "    [--event-type <Variant>]",
            "    (Variant: ToolCall, ScopeDenied, TurnStarted, TurnEnded,",
            "     MemoryAccess, AutoNotifyDispatched, SkillAutoProposal,",
            "     SkillInvocation)",
        ],
    },
    CommandHelp {
        name: "cost",
        group: Group::Inspection,
        summary: "Show the priced LLM spend report",
        usage: &["aivyx-pa cost [--today]"],
    },
    CommandHelp {
        name: "routing",
        group: Group::Inspection,
        summary: "Inspect model-routing decisions",
        usage: &[
            "aivyx-pa routing status",
            "aivyx-pa routing explain [--limit <N>]",
            "aivyx-pa routing allow-cloud <session-id>",
        ],
    },
    CommandHelp {
        name: "identity",
        group: Group::Inspection,
        summary: "Export or import the agent's federation identity",
        usage: &[
            "aivyx-pa identity export <path>",
            "aivyx-pa identity import <path> [--force]",
        ],
    },
    CommandHelp {
        name: "federation",
        group: Group::Inspection,
        summary: "Bind a hardware-backed federation key (YubiKey)",
        usage: &[
            "aivyx-pa federation yubikey-init <instance-id> <key-binding-path>",
            "    [--card <serial>] [--overwrite-existing-key]",
        ],
    },
    CommandHelp {
        name: "tool-relevance",
        group: Group::Inspection,
        summary: "Dump tool-relevance keyword scoring",
        usage: &["aivyx-pa tool-relevance dump [--keyword-key <key>]"],
    },
    CommandHelp {
        name: "workspace",
        group: Group::Inspection,
        summary: "Browse the agent's workspace directory",
        usage: &[
            "aivyx-pa workspace path",
            "aivyx-pa workspace ls [path]",
            "aivyx-pa workspace cat <path>",
        ],
    },
];

/// What [`intercept`] found in the raw argv, before any subcommand
/// dispatch or parsing runs.
pub enum HelpRequest {
    TopLevel,
    Command(&'static CommandHelp),
}

/// Look up a table entry by its dispatcher name.
pub fn command_help(name: &str) -> Option<&'static CommandHelp> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Check the raw post-binary-name argv for a help request, before
/// `parse_cli_args_from` (and therefore every subcommand's own
/// parsing) ever sees it.
///
/// - `aivyx-pa --help` / `-h` / `help` (as `args[0]`, regardless of
///   what follows) → [`HelpRequest::TopLevel`].
/// - `aivyx-pa <command> --help` / `-h` (as `args[1]`), where
///   `<command>` is a known dispatcher name → that command's
///   [`HelpRequest::Command`].
/// - Anything else → `None`, so the caller falls through to the
///   normal parse/dispatch path unchanged.
pub fn intercept(args: &[String]) -> Option<HelpRequest> {
    let first = args.first()?;
    if first == "--help" || first == "-h" || first == "help" {
        return Some(HelpRequest::TopLevel);
    }
    match args.get(1).map(String::as_str) {
        Some("--help") | Some("-h") => command_help(first).map(HelpRequest::Command),
        _ => None,
    }
}

/// Render one command's help entry: summary + usage lines.
pub fn render_command_help(cmd: &CommandHelp) -> String {
    let mut out = String::new();
    out.push_str(cmd.summary);
    out.push_str(".\n\nUsage:\n");
    for line in cmd.usage {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Render the `name  summary` lines for every [`COMMANDS`] entry in
/// `group`, in table order.
fn group_lines(group: Group) -> String {
    let mut out = String::new();
    for cmd in COMMANDS.iter().filter(|c| c.group == group) {
        out.push_str(&format!("  {:<17} {}.\n", cmd.name, cmd.summary));
    }
    out
}

/// Render the full grouped top-level help — what `aivyx-pa --help`,
/// `-h` and `help` print.
pub fn render_top_level_help() -> String {
    let mut out = String::new();
    out.push_str("aivyx-pa — a local-first autonomous agent platform.\n\n");
    out.push_str("Usage:\n");
    out.push_str("  aivyx-pa [flags]            Start an interactive chat session (REPL).\n");
    out.push_str("  aivyx-pa <command> [args]   Run a subcommand.\n");
    out.push_str("  aivyx-pa <command> --help   Show help for one command.\n\n");

    out.push_str("Chat and front ends:\n");
    out.push_str("  (no arguments)    Start an interactive chat session (REPL).\n");
    out.push_str(&group_lines(Group::FrontEnds));
    out.push_str("  --channel <kind>  Select the REPL's channel adapter (see Global flags).\n\n");

    out.push_str("Daemon:\n");
    out.push_str(&group_lines(Group::Daemon));
    out.push('\n');

    out.push_str("Setup:\n");
    out.push_str(&group_lines(Group::Setup));
    out.push('\n');

    out.push_str("The agent:\n");
    out.push_str(&group_lines(Group::Agent));
    out.push('\n');

    out.push_str("Tools and integrations:\n");
    out.push_str(&group_lines(Group::Tools));
    out.push('\n');

    out.push_str("Inspection:\n");
    out.push_str(&group_lines(Group::Inspection));
    out.push('\n');

    out.push_str("Global flags:\n");
    out.push_str("  --instance <name>      Use a named instance (multi-instance support).\n");
    out.push_str("  --verify-only          Forensic: verify the audit chain and exit.\n");
    out.push_str("  --channel <kind>       local | telegram | discord | slack | voice.\n");
    out.push_str("  --role <name>          Use a named role for this session.\n");
    out.push_str("  --print-role <name>    Print a role's rendered envelope and exit.\n");
    out.push_str("  --no-daemon            Chat in-process: don't connect to a running\n");
    out.push_str("                         daemon or start one.\n");
    out.push_str("  --provider <kind>      anthropic | openai | ollama | llamacpp | jan |\n");
    out.push_str("                         mistralrs | broker | lemonade.\n");
    out.push_str("  --mcp-server <name:command[:arg1,arg2,...]>\n");
    out.push_str("                         Attach a stdio MCP server for this session.\n");
    out.push_str("  --mcp-sse <name:url>   Attach an SSE MCP server for this session.\n");
    out.push_str("  --version, -V          Print the version and exit.\n\n");

    out.push_str("Run `aivyx-pa <command> --help` for details.\n");
    out
}

/// Point an argument-parsing error at the help that would have answered it:
/// the command's own `--help` when `args` names a known command, otherwise
/// the top-level help. Errors that aren't about an unrecognized argument or
/// subcommand, or that already mention `--help`, pass through unchanged.
pub fn with_help_hint(err: String, args: &[String]) -> String {
    if !err.contains("unrecognized") || err.contains("--help") {
        return err;
    }
    let trimmed = err.trim_end().trim_end_matches('.');
    match args.first().and_then(|name| command_help(name)) {
        Some(cmd) => format!("{trimmed}. Run `aivyx-pa {} --help` for usage.", cmd.name),
        None => format!("{trimmed}. Run `aivyx-pa --help` to see every command."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_cli_reference_heading() {
        let manual = include_str!("../../../../../docs/manual/reference/01-cli.md");
        let missing: Vec<&str> = COMMANDS
            .iter()
            .map(|c| c.name)
            .filter(|name| !manual.contains(&format!("### `aivyx-pa {name}")))
            .collect();
        assert!(
            missing.is_empty(),
            "docs/manual/reference/01-cli.md has no heading for: {missing:?}"
        );
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unrecognized_errors_point_at_the_commands_own_help() {
        let err = "unrecognized argument to `aivyx-pa routing status`: `--x`".to_string();
        assert_eq!(
            with_help_hint(err, &argv(&["routing", "status", "--x"])),
            "unrecognized argument to `aivyx-pa routing status`: `--x`. \
             Run `aivyx-pa routing --help` for usage."
        );
    }

    #[test]
    fn unrecognized_errors_outside_a_command_point_at_top_level_help() {
        let err = "unrecognized argument: `--x`".to_string();
        assert_eq!(
            with_help_hint(err, &argv(&["--x"])),
            "unrecognized argument: `--x`. Run `aivyx-pa --help` to see every command."
        );
    }

    #[test]
    fn errors_that_already_mention_help_or_are_not_parse_errors_are_untouched() {
        let hinted = "unrecognized argument: `--x`. Run `aivyx-pa --help` to see every command.";
        assert_eq!(with_help_hint(hinted.to_string(), &argv(&["--x"])), hinted);
        let other = "failed to open the store: permission denied";
        assert_eq!(with_help_hint(other.to_string(), &argv(&["memory", "list"])), other);
    }

    #[test]
    fn top_level_help_intercepts_help_flag() {
        assert!(matches!(
            intercept(&argv(&["--help"])),
            Some(HelpRequest::TopLevel)
        ));
    }

    #[test]
    fn top_level_help_intercepts_short_flag() {
        assert!(matches!(intercept(&argv(&["-h"])), Some(HelpRequest::TopLevel)));
    }

    #[test]
    fn top_level_help_intercepts_bare_help_word() {
        assert!(matches!(
            intercept(&argv(&["help"])),
            Some(HelpRequest::TopLevel)
        ));
    }

    #[test]
    fn empty_argv_is_not_a_help_request() {
        assert!(intercept(&argv(&[])).is_none());
    }

    #[test]
    fn unrelated_argv_is_not_a_help_request() {
        assert!(intercept(&argv(&["--role", "researcher"])).is_none());
    }

    #[test]
    fn command_help_flag_intercepts_known_commands() {
        for name in ["routing", "doctor", "init", "daemon", "tui"] {
            let req = intercept(&argv(&[name, "--help"]));
            assert!(
                matches!(req, Some(HelpRequest::Command(c)) if c.name == name),
                "`{name} --help` must resolve to its own entry"
            );
            let req = intercept(&argv(&[name, "-h"]));
            assert!(
                matches!(req, Some(HelpRequest::Command(c)) if c.name == name),
                "`{name} -h` must resolve to its own entry"
            );
        }
    }

    #[test]
    fn command_help_flag_ignores_unknown_commands() {
        assert!(intercept(&argv(&["bogus", "--help"])).is_none());
    }

    #[test]
    fn every_command_renders_its_summary_and_usage() {
        for cmd in COMMANDS {
            let rendered = render_command_help(cmd);
            assert!(rendered.contains(cmd.summary), "{}", cmd.name);
            for line in cmd.usage {
                assert!(rendered.contains(line), "{}: missing usage {line:?}", cmd.name);
            }
        }
    }

    #[test]
    fn top_level_help_lists_every_command_and_closes_with_the_help_hint() {
        let rendered = render_top_level_help();
        for cmd in COMMANDS {
            assert!(
                rendered.contains(cmd.name),
                "top-level help is missing `{}`",
                cmd.name
            );
        }
        assert!(rendered.contains("Run `aivyx-pa <command> --help` for details."));
    }

    #[test]
    fn help_output_fits_80_columns() {
        for line in render_top_level_help().lines() {
            assert!(line.chars().count() <= 80, "over 80 cols: {line:?}");
        }
        for cmd in COMMANDS {
            for line in render_command_help(cmd).lines() {
                assert!(line.chars().count() <= 80, "{}: over 80 cols: {line:?}", cmd.name);
            }
        }
    }
}
