//! Destructive-command detection for `shell.exec`.
//!
//! The docs and the init wizard promise that, with `confirm_destructive` on,
//! "a hallucinated `rm` asks first". This is the check behind that promise:
//! [`classify`] reads a command line and returns the reason it should ask the
//! operator first, or `None` for a command it is sure is not destructive.
//!
//! It is deliberately conservative: it is not a shell parser, so anything it
//! cannot be sure about (an unterminated quote, a command name built from a
//! variable, a destination with a glob in it) counts as destructive and asks.
//! The cost of a false positive is one approval; the cost of a miss is a
//! deleted project.
//!
//! What it looks at, in every simple command of a compound line (split on
//! `;`, `&&`, `||`, `|`, `&`, newlines, subshell parentheses, `$( )`,
//! backticks, `<( )`, and the text of `sh -c`, `eval`, `xargs`, `find -exec`
//! and the usual wrappers such as `sudo` and `env`):
//!
//! - removal: `rm`, `rmdir`, `unlink`, `shred`, `find -delete`, `rsync --delete`
//! - overwrite: `mv`/`cp`/`install`/`ln -f` onto an existing path, `tee`
//!   and `>` onto an existing file, `sed -i`, `perl -i`, `truncate`, `dd`
//! - disk and permissions: `mkfs*`, `wipefs`, `fdisk`, `parted`, `chmod -R`,
//!   `chown -R`
//! - git history and work tree: `reset --hard`, `clean`, forced or deleting
//!   `push`, `checkout -f/--/.`, `restore`, `stash drop/clear`,
//!   `branch -D`, `reflog expire`, `gc --prune`

use std::path::Path;

/// Wrappers whose remaining words are themselves a command.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "nice", "ionice", "time", "command", "exec", "builtin",
    "timeout", "setsid", "stdbuf", "xargs", "chroot", "unbuffer", "busybox", "watch", "script",
];

/// Wrapper options that take a separate value word.
const WRAPPER_VALUE_FLAGS: &[&str] = &[
    "-u", "-g", "-h", "-p", "-C", "-D", "-R", "-r", "-t", "-T", "-U", "-n", "-c", "-s", "-a", "-I",
    "-L", "-P", "-d", "-E", "-S", "-N", "--user", "--group", "--chdir",
];

/// Shell keywords that precede a command in a compound statement.
const KEYWORDS: &[&str] = &[
    "!", "if", "then", "else", "elif", "do", "while", "until", "{", "}", "fi", "done", "esac",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash", "fish"];

/// Interpreters whose code (`-c`, `-e`, a file) is invisible here.
const INTERPRETERS: &[&str] = &["python", "perl", "node", "nodejs", "ruby", "php", "lua", "deno", "bun"];

const MAX_DEPTH: usize = 6;

/// One lexed word: its text with quotes removed, and whether it contains
/// anything the shell would expand (so its real value is unknown).
#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    dynamic: bool,
    /// Wrote some quote characters (so `">"` is a word, not a redirect).
    quoted: bool,
}

#[derive(Debug, Default)]
struct Command {
    words: Vec<Word>,
    /// `(operator, target)` for `>`, `>|`, `&>` (truncating redirects).
    truncating: Vec<Word>,
}

/// Why `cmd` (run with `cwd` as its working directory) should ask first, or
/// `None` when it is not destructive as far as this check can tell.
pub fn classify(cmd: &str, cwd: &Path) -> Option<String> {
    classify_for(cmd, cwd, Mode::All)
}

/// Which destructive commands count: the postures of the file area and the
/// git area can differ, so `shell.exec` asks for git commands under the git
/// posture and for everything else under the file posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Everything.
    All,
    /// Everything except `git` subcommands.
    NoGit,
    /// Only `git` subcommands (and what could hide one).
    GitOnly,
}

/// [`classify`] for the given [`Mode`].
pub fn classify_for(cmd: &str, cwd: &Path, mode: Mode) -> Option<String> {
    let mut st = State {
        cwd,
        cwd_unknown: false,
        mode,
    };
    st.scan(cmd, 0)
}

struct State<'a> {
    mode: Mode,
    cwd: &'a Path,
    /// A `cd` ran earlier in the line, so relative paths mean nothing here.
    cwd_unknown: bool,
}

impl State<'_> {
    /// Lex `src` into simple commands and check each. Inner `$( )` and
    /// backtick text found while lexing is scanned recursively.
    fn scan(&mut self, src: &str, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return Some("the command nests too deeply to check".into());
        }
        let (commands, inner) = match lex(src) {
            Ok(x) => x,
            Err(why) => return Some(format!("the command could not be parsed safely ({why})")),
        };
        for text in inner {
            if let Some(r) = self.scan(&text, depth + 1) {
                return Some(r);
            }
        }
        for c in commands {
            if let Some(r) = self.check(&c, depth) {
                return Some(r);
            }
        }
        None
    }

    fn exists(&self, word: &Word) -> bool {
        if word.text.starts_with("/dev/") {
            return false;
        }
        let p = Path::new(&word.text);
        if p.is_relative() && self.cwd_unknown {
            return true; // unknown directory: assume the worst
        }
        std::fs::symlink_metadata(self.cwd.join(p)).is_ok()
    }

    fn is_dir(&self, word: &Word) -> bool {
        self.cwd.join(&word.text).is_dir()
    }

    fn check(&mut self, c: &Command, depth: usize) -> Option<String> {
        for target in c.truncating.iter().filter(|_| self.mode != Mode::GitOnly) {
            if target.dynamic || target.text.is_empty() {
                return Some("it redirects output (`>`) to a path it cannot resolve".into());
            }
            if self.exists(target) {
                return Some(format!(
                    "it overwrites the existing file `{}` with `>`",
                    target.text
                ));
            }
        }
        let mut words: &[Word] = &c.words;
        // Leading `VAR=value` assignments and compound-statement keywords.
        while let Some(w) = words.first() {
            let t = w.text.as_str();
            let assignment = !w.quoted
                && t.split_once('=').is_some_and(|(k, _)| {
                    !k.is_empty()
                        && k.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                        && !k.starts_with(|ch: char| ch.is_ascii_digit())
                });
            if assignment || (!w.quoted && KEYWORDS.contains(&t)) {
                words = &words[1..];
            } else {
                break;
            }
        }
        let first = words.first()?;
        if first.dynamic {
            return Some(
                "the command name comes from an expansion, so it cannot be checked".into(),
            );
        }
        // A script or program named by path (`./clean.sh`, `../bin/tool`,
        // `/home/me/x.sh`) is code this check cannot see. System binary
        // directories are the exception: they are named by what they are.
        if first.text.contains('/') {
            let dir = first.text.rsplit_once('/').map_or("", |(d, _)| d);
            const SYSTEM_BINS: &[&str] = &[
                "/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/local/bin", "/usr/local/sbin",
            ];
            if !SYSTEM_BINS.contains(&dir) {
                return Some(format!(
                    "`{}` runs a program or script this check cannot read",
                    first.text
                ));
            }
        }
        let name = first.text.rsplit('/').next().unwrap_or(&first.text).to_string();
        let args = &words[1..];
        self.check_named(&name, args, depth)
    }

    fn check_named(&mut self, name: &str, args: &[Word], depth: usize) -> Option<String> {
        let flag = |w: &Word| !w.quoted && w.text.starts_with('-') && w.text != "-";
        let has_flag = |short: char, long: &str| {
            args.iter().any(|w| {
                flag(w)
                    && ((w.text.starts_with("--") && w.text == long)
                        || (!w.text.starts_with("--") && w.text[1..].contains(short)))
            })
        };
        let positionals = || {
            let mut out = Vec::new();
            let mut options_done = false;
            for w in args {
                if !options_done && w.text == "--" && !w.quoted {
                    options_done = true;
                } else if options_done || !flag(w) {
                    out.push(w.clone());
                }
            }
            out
        };

        if name == "cd" || name == "pushd" || name == "popd" {
            self.cwd_unknown = true;
            return None;
        }

        // Wrappers: the rest of the words are a command.
        if WRAPPERS.contains(&name) {
            let mut i = 0;
            while i < args.len() {
                let w = &args[i];
                if name == "env" && w.text.contains('=') && !w.text.starts_with('-') {
                    i += 1;
                } else if flag(w) {
                    let takes_value = WRAPPER_VALUE_FLAGS.contains(&w.text.as_str());
                    i += 1 + usize::from(takes_value);
                } else if name == "timeout" && i == 0 {
                    i += 1; // the duration
                } else {
                    break;
                }
            }
            if i >= args.len() {
                return None;
            }
            let rest = Command {
                words: args[i..].to_vec(),
                truncating: Vec::new(),
            };
            return self.check(&rest, depth + 1);
        }

        // In git-only mode nothing below matters except the shells and `eval`
        // (which can carry a git command) and `git` itself.
        if self.mode == Mode::GitOnly
            && !SHELLS.contains(&name)
            && name != "eval"
            && name != "git"
        {
            return None;
        }
        if self.mode == Mode::NoGit && name == "git" {
            return None;
        }

        // `sh -c "..."`, `eval ...`.
        if SHELLS.contains(&name) {
            for (i, w) in args.iter().enumerate() {
                if flag(w) && !w.text.starts_with("--") && w.text[1..].contains('c') {
                    return match args.get(i + 1) {
                        Some(script) if !script.dynamic => self.scan(&script.text, depth + 1),
                        _ => Some("a shell script string it cannot see".into()),
                    };
                }
            }
            if self.mode == Mode::GitOnly {
                return None;
            }
            // No `-c`: the shell reads a script file or stdin (`sh x.sh`,
            // `... | sh`, `sh < x`), which this check cannot see.
            if args.iter().all(|w| w.text == "--version" || w.text == "-h" || w.text == "--help") && !args.is_empty() {
                return None;
            }
            return Some(format!("`{name}` runs a script or stdin this check cannot read"));
        }
        if INTERPRETERS.iter().any(|i| name == *i || name.strip_prefix(*i).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.')))
        {
            if !args.is_empty()
                && args.iter().all(|w| matches!(w.text.as_str(), "--version" | "-V" | "-v" | "-h" | "--help"))
            {
                return None;
            }
            return Some(format!("`{name}` runs code this check cannot read"));
        }
        if matches!(name, ".") || name == "source" {
            return Some("`source` runs a script this check cannot read".into());
        }
        if matches!(name, "ex" | "ed" | "vim" | "vi" | "nvim" | "view") {
            return Some(format!("`{name}` can rewrite files from a script"));
        }
        if name == "crontab" {
            return (!args.iter().all(|w| w.text == "-l"))
                .then(|| "`crontab` replaces or removes the scheduled jobs".into());
        }
        if name == "eval" {
            if args.iter().any(|w| w.dynamic) {
                return Some("`eval` of an expansion cannot be checked".into());
            }
            let joined: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
            return self.scan(&joined.join(" "), depth + 1);
        }

        match name {
            "rm" | "rmdir" | "unlink" | "shred" => {
                Some(format!("`{name}` removes files, and that cannot be undone"))
            }
            "dd" => args
                .iter()
                .any(|w| w.text.starts_with("of="))
                .then(|| "`dd` writes over its output target".into()),
            "truncate" => Some("`truncate` cuts a file down or empties it".into()),
            "wipefs" | "fdisk" | "sfdisk" | "parted" | "gdisk" | "blkdiscard" => {
                Some(format!("`{name}` changes a disk's layout or wipes it"))
            }
            _ if name.starts_with("mkfs") || name == "mke2fs" => {
                Some(format!("`{name}` formats a device"))
            }
            "chmod" | "chown" | "chgrp" => (has_flag('R', "--recursive"))
                .then(|| format!("`{name} -R` changes permissions across a whole tree")),
            "find" => self.check_find(args, depth),
            "rsync" => {
                if args
                    .iter()
                    .any(|w| w.text.starts_with("--delete") || w.text == "--remove-source-files")
                {
                    return Some("`rsync` is set to delete files".into());
                }
                let pos = positionals();
                let dest = pos.last()?;
                if pos.len() >= 2 && !dest.text.contains(':') && (dest.dynamic || self.exists(dest)) {
                    return Some(format!("`rsync` writes into the existing `{}`", dest.text));
                }
                None
            }
            "sed" | "perl" | "ruby" => args
                .iter()
                .any(|w| {
                    flag(w)
                        && (w.text.starts_with("--in-place")
                            || (!w.text.starts_with("--")
                                && w.text[1..].starts_with(|c: char| c.is_ascii_alphabetic())
                                && w.text[1..].chars().take_while(|c| c.is_ascii_alphabetic()).any(|c| c == 'i')))
                })
                .then(|| format!("`{name} -i` rewrites files in place")),
            "awk" | "gawk" => args
                .iter()
                .enumerate()
                .any(|(i, w)| {
                    w.text == "-iinplace"
                        || w.text == "--include=inplace"
                        || (w.text == "-i" && args.get(i + 1).is_some_and(|n| n.text == "inplace"))
                })
                .then(|| format!("`{name} -i inplace` rewrites files in place")),
            "curl" | "wget" | "sort" => self.check_output_flags(name, args),
            "tee" => {
                if has_flag('a', "--append") {
                    return None;
                }
                positionals()
                    .iter()
                    .find(|w| w.dynamic || self.exists(w))
                    .map(|w| format!("`tee` overwrites `{}`", w.text))
            }
            "mv" | "cp" | "install" | "ln" => self.check_move_like(name, args, &positionals(), &flag),
            "git" => self.check_git(args),
            _ => None,
        }
    }

    /// `-o`/`-O`/`--output` on a tool that then overwrites that file.
    fn check_output_flags(&self, name: &str, args: &[Word]) -> Option<String> {
        let (shorts, longs): (&[&str], &[&str]) = match name {
            "curl" => (&["-o"], &["--output"]),
            "wget" => (&["-O"], &["--output-document"]),
            _ => (&["-o"], &["--output"]),
        };
        for (i, w) in args.iter().enumerate() {
            let t = w.text.as_str();
            if name == "curl" && (t == "-O" || t == "--remote-name" || t == "--remote-name-all") {
                return Some("`curl -O` writes a file named by the server".into());
            }
            let value: Option<Word> = if shorts.contains(&t) || longs.contains(&t) {
                args.get(i + 1).cloned()
            } else if let Some(v) = longs.iter().find_map(|l| t.strip_prefix(&format!("{l}="))) {
                Some(Word { text: v.to_string(), dynamic: w.dynamic, quoted: w.quoted })
            } else if let Some(v) = shorts.iter().find_map(|sh| t.strip_prefix(sh)).filter(|v| !v.is_empty() && !t.starts_with("--")) {
                Some(Word { text: v.to_string(), dynamic: w.dynamic, quoted: w.quoted })
            } else {
                continue;
            };
            let Some(v) = value else {
                return Some(format!("`{name}` has an output flag with no visible target"));
            };
            if v.text == "-" || v.text.starts_with("/dev/") {
                continue;
            }
            if v.dynamic || self.exists(&v) {
                return Some(format!("`{name}` would overwrite `{}`", v.text));
            }
        }
        None
    }

    fn check_find(&mut self, args: &[Word], depth: usize) -> Option<String> {
        if args.iter().any(|w| w.text == "-delete") {
            return Some("`find -delete` removes every match".into());
        }
        let mut i = 0;
        while i < args.len() {
            if matches!(args[i].text.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                let rest: Vec<Word> = args[i + 1..]
                    .iter()
                    .take_while(|w| w.text != ";" && w.text != "+")
                    .cloned()
                    .collect();
                if rest.is_empty() {
                    return Some("`find -exec` with no visible command".into());
                }
                let cmd = Command {
                    words: rest,
                    truncating: Vec::new(),
                };
                if let Some(r) = self.check(&cmd, depth + 1) {
                    return Some(format!("`find -exec` runs a destructive command: {r}"));
                }
            }
            i += 1;
        }
        None
    }

    fn check_move_like(
        &self,
        name: &str,
        args: &[Word],
        positionals: &[Word],
        flag: &dyn Fn(&Word) -> bool,
    ) -> Option<String> {
        let short_has = |c: char| {
            args.iter()
                .any(|w| flag(w) && !w.text.starts_with("--") && w.text[1..].contains(c))
        };
        let long_has = |l: &str| args.iter().any(|w| w.text == l);
        if name == "ln" && !(short_has('f') || long_has("--force")) {
            return None;
        }
        if name != "ln" && (short_has('n') || long_has("--no-clobber")) {
            return None;
        }
        // `-t DIR` / `--target-directory=DIR` names the destination.
        let mut target_dir: Option<Word> = None;
        for (i, w) in args.iter().enumerate() {
            if w.text == "-t" || w.text == "--target-directory" {
                target_dir = args.get(i + 1).cloned();
            } else if let Some(v) = w.text.strip_prefix("--target-directory=") {
                target_dir = Some(Word {
                    text: v.to_string(),
                    dynamic: w.dynamic,
                    quoted: w.quoted,
                });
            }
        }
        let (sources, dest): (Vec<Word>, Word) = match target_dir {
            Some(d) => {
                // The `-t` value was also picked up as a positional.
                let srcs = positionals.iter().filter(|w| w.text != d.text).cloned().collect();
                (srcs, d)
            }
            None => {
                if positionals.len() < 2 {
                    return None;
                }
                let (d, s) = positionals.split_last()?;
                (s.to_vec(), d.clone())
            }
        };
        if dest.dynamic {
            return Some(format!(
                "`{name}` writes to a destination (`{}`) it cannot resolve",
                dest.text
            ));
        }
        if !self.exists(&dest) {
            return None;
        }
        if !self.is_dir(&dest) || self.cwd_unknown {
            return Some(format!("`{name}` would replace the existing `{}`", dest.text));
        }
        for s in &sources {
            if s.dynamic {
                return Some(format!(
                    "`{name}` copies unknown names into the existing `{}`",
                    dest.text
                ));
            }
            let base = s.text.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            if self.cwd.join(&dest.text).join(base).exists() {
                return Some(format!(
                    "`{name}` would replace `{}/{base}`",
                    dest.text.trim_end_matches('/')
                ));
            }
        }
        None
    }

    fn check_git(&self, args: &[Word]) -> Option<String> {
        // Skip global options (`-C dir`, `-c k=v`, `--git-dir=...`).
        let mut i = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            if matches!(t, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                i += 2;
            } else if t.starts_with('-') {
                i += 1;
            } else {
                break;
            }
        }
        let sub = args.get(i)?;
        if sub.dynamic {
            return Some("the git subcommand comes from an expansion".into());
        }
        let rest = &args[i + 1..];
        let any = |f: &dyn Fn(&str) -> bool| rest.iter().any(|w| f(&w.text));
        let reason = match sub.text.as_str() {
            "reset" => any(&|t| t == "--hard" || t == "--merge" || t == "--keep")
                .then_some("`git reset --hard` discards uncommitted work"),
            "clean" => (!any(&|t| t == "-n" || t == "--dry-run"))
                .then_some("`git clean` deletes untracked files"),
            "push" => any(&|t| {
                t.starts_with(':')
                    || t == "-f"
                    || t == "--force"
                    || t.starts_with("--force-with-lease")
                    || t == "--delete"
                    || t == "-d"
                    || t == "--mirror"
                    || t == "--prune"
                    || (t.starts_with('+') && t.len() > 1)
                    || (t.starts_with('-') && !t.starts_with("--") && t[1..].contains('f'))
            })
            .then_some("`git push` would force-update or delete remote history"),
            "checkout" => {
                let path_arg = rest.iter().any(|w| {
                    !w.text.starts_with('-') && (w.dynamic || self.cwd.join(&w.text).exists())
                });
                (any(&|t| t == "-f" || t == "--force" || t == "--" || t == "." || t == "-B") || path_arg)
                    .then_some("`git checkout` would overwrite working-tree changes")
            }
            "switch" => any(&|t| t == "-f" || t == "--force" || t == "--discard-changes")
                .then_some("`git switch` would discard working-tree changes"),
            "restore" => Some("`git restore` overwrites working-tree changes"),
            "rm" => Some("`git rm` deletes files from the work tree"),
            "worktree" => any(&|t| t == "remove" || t == "prune")
                .then_some("`git worktree remove` deletes a working tree"),
            "stash" => any(&|t| t == "drop" || t == "clear")
                .then_some("`git stash drop/clear` loses stashed work"),
            "branch" => any(&|t| t == "-D" || t == "--delete" || t == "-d" || t == "-M" || t == "-f")
                .then_some("`git branch` would delete or move a branch"),
            "tag" => any(&|t| t == "-d" || t == "--delete" || t == "-f")
                .then_some("`git tag` would delete or move a tag"),
            "reflog" => any(&|t| t == "expire" || t == "delete")
                .then_some("`git reflog` would drop recovery history"),
            "gc" => any(&|t| t.starts_with("--prune"))
                .then_some("`git gc --prune` removes unreachable objects"),
            "update-ref" => any(&|t| t == "-d").then_some("`git update-ref -d` deletes a ref"),
            "rebase" | "filter-branch" | "filter-repo" => {
                Some("it rewrites git history")
            }
            _ => None,
        };
        reason.map(str::to_string)
    }
}

/// Lex one command line into simple commands. Returns the commands and the
/// text of every command substitution found (to be scanned separately).
/// `Err` when the line cannot be lexed with confidence.
#[allow(unused_assignments)] // the final `end_command!` resets state that is then dropped
fn lex(src: &str) -> Result<(Vec<Command>, Vec<String>), &'static str> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut commands: Vec<Command> = Vec::new();
    let mut inner: Vec<String> = Vec::new();
    let mut cur = Command::default();
    let mut word = Word::default();
    let mut in_word = false;
    // The next completed word is the target of a truncating redirect, or an
    // ignored redirect target (`<`, `>>`, `>&`).
    #[derive(PartialEq)]
    enum Pending {
        None,
        Truncate,
        Ignore,
    }
    let mut pending = Pending::None;
    // Heredoc delimiters whose bodies start after the next newline:
    // (delimiter, quoted?).
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut heredoc_next = false;

    macro_rules! end_word {
        () => {
            if in_word {
                let w = std::mem::take(&mut word);
                in_word = false;
                if heredoc_next {
                    heredocs.push((w.text.clone(), w.quoted));
                    heredoc_next = false;
                } else {
                    match std::mem::replace(&mut pending, Pending::None) {
                        Pending::Truncate => cur.truncating.push(w),
                        Pending::Ignore => {}
                        Pending::None => cur.words.push(w),
                    }
                }
            }
        };
    }
    macro_rules! end_command {
        () => {
            end_word!();
            if pending != Pending::None {
                return Err("a redirect with no target");
            }
            let c = std::mem::take(&mut cur);
            if !c.words.is_empty() || !c.truncating.is_empty() {
                commands.push(c);
            }
        };
    }

    while i < chars.len() {
        let ch = chars[i];
        match ch {
            ' ' | '\t' => {
                end_word!();
                i += 1;
            }
            '\n' => {
                end_command!();
                i += 1;
                // Heredoc bodies start on the next line.
                for (delim, quoted) in std::mem::take(&mut heredocs) {
                    let mut body = String::new();
                    let mut found = false;
                    while i < chars.len() {
                        let start = i;
                        while i < chars.len() && chars[i] != '\n' {
                            i += 1;
                        }
                        let line: String = chars[start..i].iter().collect();
                        if i < chars.len() {
                            i += 1;
                        }
                        if line.trim_start_matches('\t') == delim {
                            found = true;
                            break;
                        }
                        body.push_str(&line);
                        body.push('\n');
                    }
                    if !found {
                        return Err("an unterminated here-document");
                    }
                    if !quoted && (body.contains("$(") || body.contains('`')) {
                        inner.push(body);
                    }
                }
            }
            ';' | '|' | '(' | ')' => {
                end_command!();
                i += 1;
            }
            '&' => {
                if chars.get(i + 1) == Some(&'>') {
                    // `&>file` / `&>>file`
                    end_word!();
                    i += 2;
                    if chars.get(i) == Some(&'>') {
                        i += 1;
                        pending = Pending::Ignore;
                    } else {
                        pending = Pending::Truncate;
                    }
                } else {
                    end_command!();
                    i += 1;
                }
            }
            '#' if !in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '>' => {
                // A bare number just before is the fd (`2>`), not an argument.
                if in_word
                    && !word.quoted
                    && !word.dynamic
                    && !word.text.is_empty()
                    && word.text.chars().all(|c| c.is_ascii_digit())
                {
                    word = Word::default();
                    in_word = false;
                } else {
                    end_word!();
                }
                i += 1;
                match chars.get(i) {
                    Some('>') => {
                        i += 1;
                        pending = Pending::Ignore; // append
                    }
                    Some('&') => {
                        i += 1;
                        // `>&2` / `>&-` duplicate an fd; `>&file` truncates.
                        let dup = chars
                            .get(i)
                            .is_some_and(|c| c.is_ascii_digit() || *c == '-');
                        pending = if dup { Pending::Ignore } else { Pending::Truncate };
                    }
                    Some('|') => {
                        i += 1;
                        pending = Pending::Truncate;
                    }
                    _ => pending = Pending::Truncate,
                }
            }
            '<' => {
                if in_word
                    && !word.quoted
                    && !word.dynamic
                    && word.text.chars().all(|c| c.is_ascii_digit())
                {
                    word = Word::default();
                    in_word = false;
                } else {
                    end_word!();
                }
                i += 1;
                match chars.get(i) {
                    Some('<') => {
                        i += 1;
                        if chars.get(i) == Some(&'<') {
                            i += 1; // here-string: the next word is data
                            pending = Pending::Ignore;
                        } else {
                            if chars.get(i) == Some(&'-') {
                                i += 1;
                            }
                            heredoc_next = true;
                        }
                    }
                    Some('(') => {
                        // Process substitution `<(cmd)`.
                        let (text, next) = matching_paren(&chars, i + 1)?;
                        inner.push(text);
                        i = next;
                    }
                    Some('&') => {
                        i += 1;
                        pending = Pending::Ignore;
                    }
                    _ => pending = Pending::Ignore,
                }
            }
            '\\' => {
                in_word = true;
                word.quoted = true;
                match chars.get(i + 1) {
                    Some('\n') => {
                        i += 2;
                        if word.text.is_empty() && !word.dynamic {
                            in_word = false;
                            word.quoted = false;
                        }
                    }
                    Some(c) => {
                        word.text.push(*c);
                        i += 2;
                    }
                    None => return Err("a trailing backslash"),
                }
            }
            '\'' => {
                in_word = true;
                word.quoted = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(c) => {
                            word.text.push(*c);
                            i += 1;
                        }
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                word.quoted = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => match chars.get(i + 1) {
                            Some(c) => {
                                if !matches!(c, '"' | '\\' | '$' | '`') {
                                    word.text.push('\\');
                                }
                                word.text.push(*c);
                                i += 2;
                            }
                            None => return Err("an unterminated quote"),
                        },
                        Some('$') if chars.get(i + 1) == Some(&'(') => {
                            let (text, next) = matching_paren(&chars, i + 2)?;
                            inner.push(text);
                            word.dynamic = true;
                            word.text.push_str("$()");
                            i = next;
                        }
                        Some('`') => {
                            let (text, next) = matching_tick(&chars, i + 1)?;
                            inner.push(text);
                            word.dynamic = true;
                            word.text.push_str("``");
                            i = next;
                        }
                        Some('$') => {
                            if chars
                                .get(i + 1)
                                .is_some_and(|c| c.is_ascii_alphanumeric() || "_{@*#?!-".contains(*c))
                            {
                                word.dynamic = true;
                            }
                            word.text.push('$');
                            i += 1;
                        }
                        Some(c) => {
                            word.text.push(*c);
                            i += 1;
                        }
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            '`' => {
                in_word = true;
                let (text, next) = matching_tick(&chars, i + 1)?;
                inner.push(text);
                word.dynamic = true;
                word.text.push_str("``");
                i = next;
            }
            '$' => {
                in_word = true;
                if chars.get(i + 1) == Some(&'(') {
                    let (text, next) = matching_paren(&chars, i + 2)?;
                    inner.push(text);
                    word.dynamic = true;
                    word.text.push_str("$()");
                    i = next;
                } else {
                    if chars
                        .get(i + 1)
                        .is_some_and(|c| c.is_ascii_alphanumeric() || "_{@*#?!-'\"".contains(*c))
                    {
                        word.dynamic = true;
                    }
                    word.text.push('$');
                    i += 1;
                }
            }
            '*' | '?' | '[' | '{' | '}' | '~' => {
                // A lone `{`/`}` is a compound-statement keyword; elsewhere
                // these are glob, brace or tilde expansions.
                let starts = !in_word;
                in_word = true;
                word.text.push(ch);
                let keyword = starts && (ch == '{' || ch == '}');
                let tilde_mid_word = ch == '~' && !starts;
                if !keyword && !tilde_mid_word {
                    word.dynamic = true;
                }
                i += 1;
            }
            c => {
                in_word = true;
                word.text.push(c);
                i += 1;
            }
        }
    }
    if heredoc_next || !heredocs.is_empty() {
        return Err("a here-document with no body");
    }
    end_command!();
    Ok((commands, inner))
}

/// The text inside `( ... )` starting just after the opening paren; returns
/// it and the index after the closing paren.
fn matching_paren(chars: &[char], mut i: usize) -> Result<(String, usize), &'static str> {
    let start = i;
    let mut depth = 1;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((chars[start..i].iter().collect(), i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err("an unterminated `$(`")
}

fn matching_tick(chars: &[char], mut i: usize) -> Result<(String, usize), &'static str> {
    let start = i;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '`' => return Ok((chars[start..i].iter().collect(), i + 1)),
            _ => {}
        }
        i += 1;
    }
    Err("an unterminated backtick")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aivyx-destructive-{}-{}",
            std::process::id(),
            uuid_like()
        ));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("exists.txt"), "x").unwrap();
        d
    }

    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    }

    fn asks(cmd: &str) -> bool {
        let d = scratch();
        let r = classify(cmd, &d);
        let _ = std::fs::remove_dir_all(&d);
        r.is_some()
    }

    #[test]
    fn harmless_commands_do_not_ask() {
        for c in [
            "ls",
            "ls -la /tmp",
            "echo hello > new-file.txt",
            "echo hi >> exists.txt",
            "cargo test 2>&1 | tail -5",
            "cat exists.txt | grep x > /dev/null",
            "git status && git diff",
            "git log --oneline -5",
            "git push origin main",
            "cp exists.txt fresh.txt",
            "mv exists.txt moved.txt",
            "mkdir -p a/b && cd a/b && ls",
            "echo 'rm -rf /' # not run",
            "grep -rn \"rm -rf\" .",
            "find . -name '*.rs' -print",
            "sed -n 1,5p exists.txt",
            "cargo build 2>/dev/null",
            "echo $HOME",
            "FOO=1 make test",
            "for f in a b; do echo $f; done",
            "cat <<EOF\nhello rm -rf\nEOF",
        ] {
            assert!(!asks(c), "should not ask: {c:?}");
        }
    }

    #[test]
    fn the_audit_reproduction_asks() {
        assert!(asks("echo ok && rm -rf victim && echo done"));
    }

    #[test]
    fn removal_anywhere_in_a_compound_command_asks() {
        for c in [
            "rm file",
            "rm -rf victim",
            "/bin/rm x",
            "ls; rm x",
            "ls || rm x",
            "ls | xargs rm",
            "echo a\nrm b",
            "echo $(rm x)",
            "echo `rm x`",
            "echo \"$(rm x)\"",
            "(rm x)",
            "{ rm x; }",
            "if true; then rm x; fi",
            "for f in *; do rm $f; done",
            "sudo rm x",
            "env FOO=1 rm x",
            "nohup rm x &",
            "timeout 5 rm x",
            "sh -c 'rm x'",
            "bash -lc \"ls && rm x\"",
            "eval 'rm x'",
            "find . -name '*.o' -delete",
            "find . -exec rm {} \\;",
            "find . -type f -execdir rm -f {} +",
            "rmdir d",
            "shred -u f",
            "\\rm x",
            "'rm' x",
            "r\"m\" x",
            "cat <(rm x)",
        ] {
            assert!(asks(c), "should ask: {c:?}");
        }
    }

    #[test]
    fn overwrites_ask_only_when_something_would_be_replaced() {
        assert!(asks("echo hi > exists.txt"));
        assert!(asks("echo hi >| exists.txt"));
        assert!(asks("echo hi &> exists.txt"));
        assert!(asks("echo hi 2> exists.txt"));
        assert!(asks(": > exists.txt"));
        assert!(asks("> exists.txt"));
        assert!(asks("echo hi > $OUT"));
        assert!(!asks("echo hi > brand-new.txt"));
        assert!(!asks("echo hi > /dev/null"));
        assert!(asks("mv a exists.txt"));
        assert!(asks("cp a exists.txt"));
        assert!(asks("mv -f a exists.txt"));
        assert!(!asks("mv -n a exists.txt"));
        assert!(asks("cp exists.txt sub/../exists.txt"));
        assert!(asks("mv a $DEST"));
        assert!(asks("mv a sub/ && cd sub && mv b exists.txt"));
        assert!(asks("echo x | tee exists.txt"));
        assert!(!asks("echo x | tee -a exists.txt"));
        assert!(!asks("echo x | tee fresh.txt"));
        assert!(asks("sed -i s/a/b/ exists.txt"));
        assert!(asks("sed -i.bak s/a/b/ exists.txt"));
        assert!(asks("perl -pi -e s/a/b/ exists.txt"));
        assert!(asks("truncate -s 0 exists.txt"));
        assert!(asks("dd if=/dev/zero of=exists.txt"));
        assert!(!asks("dd if=exists.txt"));
        assert!(asks("ln -sf a exists.txt"));
        assert!(!asks("ln -s a exists.txt"));
    }

    #[test]
    fn moving_into_an_existing_directory_checks_the_name() {
        let d = scratch();
        std::fs::write(d.join("sub/exists.txt"), "y").unwrap();
        assert!(classify("cp exists.txt sub/", &d).is_some());
        assert!(classify("cp exists.txt sub", &d).is_some());
        assert!(classify("cp new.txt sub/", &d).is_none());
        assert!(classify("cp *.txt sub/", &d).is_some());
        assert!(classify("cp -t sub exists.txt", &d).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn disk_permission_and_git_commands_ask() {
        for c in [
            "mkfs.ext4 /dev/sda1",
            "wipefs -a /dev/sda",
            "chmod -R 777 .",
            "chown -R a:b .",
            "git reset --hard HEAD~1",
            "git -C repo reset --hard",
            "git clean -fd",
            "git push --force",
            "git push -f origin main",
            "git push origin +main",
            "git push origin --delete old",
            "git checkout -- .",
            "git checkout -f",
            "git restore file",
            "git stash drop",
            "git branch -D old",
            "git rebase main",
            "rsync -a --delete a/ b/",
        ] {
            assert!(asks(c), "should ask: {c:?}");
        }
        assert!(!asks("chmod +x run.sh"));
        assert!(!asks("git clean -n"));
        assert!(!asks("git checkout main"));
        assert!(!asks("git stash"));
        assert!(!asks("git branch -a"));
    }

    #[test]
    fn anything_it_cannot_parse_asks() {
        for c in [
            "echo 'unterminated",
            "echo \"unterminated",
            "$CMD x",
            "echo $(unterminated",
            "echo `unterminated",
            "eval $X",
            "sh -c \"$X\"",
            "cat <<EOF\nno end",
            "echo hi >",
            "$(echo rm) x",
        ] {
            assert!(asks(c), "should ask: {c:?}");
        }
    }

    #[test]
    fn commands_that_run_code_it_cannot_see_ask() {
        for c in [
            "sh x.sh",
            "bash x.sh",
            "sh",
            "echo rm | sh",
            "sh < x.sh",
            "./x.sh",
            "../tools/clean",
            "/home/me/clean.sh",
            ". ./env.sh",
            "source env.sh",
            "python3 -c 'import os'",
            "python x.py",
            "perl -e 'unlink 1'",
            "perl x.pl",
            "node -e 'x()'",
            "node x.js",
            "ruby x.rb",
            "php -r 'x();'",
            "echo x | python3",
            "ex -s file",
            "vim -es file",
            "crontab -r",
            "crontab mycron",
        ] {
            assert!(asks(c), "should ask: {c:?}");
        }
        for c in ["/usr/bin/ls", "python3 --version", "crontab -l", "sh --version"] {
            assert!(!asks(c), "should not ask: {c:?}");
        }
    }

    #[test]
    fn more_git_and_output_flag_forms_ask() {
        let d = scratch();
        std::fs::write(d.join("out.bin"), "x").unwrap();
        for c in [
            "git push origin :old-branch",
            "git push --delete origin old",
            "git checkout -- exists.txt",
            "git checkout exists.txt",
            "git checkout $X",
            "git restore exists.txt",
            "git rm exists.txt",
            "git worktree remove wt",
            "git worktree remove -f wt",
            "git switch --discard-changes main",
            "curl -o out.bin http://x",
            "curl --output out.bin http://x",
            "curl -oout.bin http://x",
            "curl -O http://x/a",
            "wget -O out.bin http://x",
            "sort -o out.bin in",
            "sort --output=out.bin in",
            "curl -o $F http://x",
            "awk -i inplace '{print}' exists.txt",
            "sed -i s/a/b/ exists.txt",
            "rsync -a src/ exists.txt",
            "rsync -a src/ sub/",
        ] {
            assert!(classify(c, &d).is_some(), "should ask: {c:?}");
        }
        for c in [
            "git checkout main",
            "git checkout -b feature",
            "git push origin main",
            "curl -o fresh.bin http://x",
            "curl http://x",
            "sort in",
            "awk '{print}' exists.txt",
            "rsync -a src/ newdir/",
            "git status",
        ] {
            assert!(classify(c, &d).is_none(), "should not ask: {c:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
