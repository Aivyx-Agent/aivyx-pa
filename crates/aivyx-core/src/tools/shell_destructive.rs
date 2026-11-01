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

/// Wrappers whose remaining words are themselves a command, with the options
/// of each that take a separate value word.
const WRAPPERS: &[(&str, &[&str])] = &[
    ("sudo", &["-u", "-g", "-h", "-p", "-C", "-D", "-R", "-r", "-t", "-T", "-U"]),
    ("doas", &["-u", "-C"]),
    ("env", &["-u", "-C", "-S"]),
    ("nohup", &[]),
    ("nice", &["-n"]),
    ("ionice", &["-c", "-n", "-p", "-P", "-u"]),
    ("time", &["-f", "-o"]),
    ("command", &[]),
    ("exec", &["-a"]),
    ("builtin", &[]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("setsid", &[]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("xargs", &["-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a"]),
    ("chroot", &["-u", "-g"]),
    ("unbuffer", &[]),
    ("busybox", &[]),
    ("watch", &["-n", "-d"]),
];

/// Wrappers and tools that run a command through option syntax too varied to
/// skip reliably (or read it from stdin): they ask.
const OPAQUE_WRAPPERS: &[&str] = &[
    "parallel", "flock", "script", "su", "runuser", "unshare", "fakeroot", "proot", "firejail",
    "bwrap", "chrt", "taskset", "strace", "ltrace", "valgrind", "systemd-run", "at", "batch",
    "gdb", "nsenter", "setpriv", "cpulimit", "dbus-launch", "ssh-agent", "tmux", "screen",
];

/// Shell keywords that precede a command in a compound statement.
const KEYWORDS: &[&str] = &[
    "!", "if", "then", "else", "elif", "do", "while", "until", "{", "}", "fi", "done", "esac",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash", "fish"];

/// Interpreters whose code (`-c`, `-e`, a file) is invisible here.
const INTERPRETERS: &[&str] = &[
    "python", "perl", "node", "nodejs", "ruby", "php", "lua", "deno", "bun", "julia", "pwsh",
    "powershell", "tclsh", "wish", "Rscript", "R", "osascript", "expect", "groovy", "scala",
];

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
        git_env_risky: false,
        mode,
    };
    st.scan(cmd, 0)
}

struct State<'a> {
    mode: Mode,
    cwd: &'a Path,
    /// A `cd` ran earlier in the line, so relative paths mean nothing here.
    cwd_unknown: bool,
    /// An assignment earlier on this command (`VAR=x git ...`, `env VAR=x git
    /// ...`) sets an environment variable git runs a program from.
    git_env_risky: bool,
}

/// Environment variables that make git (or the pager and editor it starts)
/// run a program: `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n` and
/// `GIT_CONFIG_PARAMETERS` set any config key, the rest name the program.
fn is_risky_git_env(name: &str) -> bool {
    matches!(
        name,
        "GIT_CONFIG_COUNT"
            | "GIT_CONFIG_PARAMETERS"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_PAGER"
            | "PAGER"
            | "GIT_EDITOR"
            | "EDITOR"
            | "VISUAL"
            | "GIT_SSH"
            | "GIT_SSH_COMMAND"
            | "GIT_ASKPASS"
            | "SSH_ASKPASS"
            | "GIT_CONFIG_GLOBAL"
            | "GIT_CONFIG_SYSTEM"
            | "GIT_PROXY_COMMAND"
    ) || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
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
        // `git_env_risky` is NOT reset per command: `X=... ; git ...` and
        // `export X=...; git ...` set the variable for the git that follows.
        // Leading `VAR=value` assignments and compound-statement keywords.
        while let Some(w) = words.first() {
            let t = w.text.as_str();
            // A quoted value (`X='a b' cmd`) makes the word `quoted`, so a
            // quote does not rule an assignment out.
            let assignment = t.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty()
                    && k.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    && !k.starts_with(|ch: char| ch.is_ascii_digit())
            });
            if assignment || (!w.quoted && KEYWORDS.contains(&t)) {
                if assignment && is_risky_git_env(t.split('=').next().unwrap_or("")) {
                    self.git_env_risky = true;
                }
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

        // `export X=...`, `declare -x X=...`, `typeset`, `readonly`, `local`:
        // the variable stays set for every later command on the line.
        if matches!(name, "export" | "declare" | "typeset" | "readonly" | "local") {
            if args
                .iter()
                .any(|w| is_risky_git_env(w.text.split('=').next().unwrap_or("")))
            {
                self.git_env_risky = true;
            }
            return None;
        }

        if name == "cd" || name == "pushd" || name == "popd" {
            self.cwd_unknown = true;
            return None;
        }

        // `command -v rm` / `-V` only describes a name; it runs nothing.
        if name == "command"
            && args
                .iter()
                .take_while(|w| flag(w))
                .any(|w| !w.text.starts_with("--") && (w.text.contains('v') || w.text.contains('V')))
        {
            return None;
        }
        // `env -S 'rm x'` / `--split-string=...` splits a STRING into the
        // command to run: not a word list this check can walk. Only the options
        // before the wrapped command count (`env ls -S` is `ls -S`).
        if name == "env" {
            let mut i = 0;
            while i < args.len() {
                let t = args[i].text.as_str();
                if t.contains('=') && !t.starts_with('-') {
                    i += 1; // VAR=value
                } else if t.starts_with('-') && t != "-" {
                    if t == "--split-string"
                        || t.starts_with("--split-string=")
                        || (!t.starts_with("--") && t[1..].contains('S'))
                    {
                        return Some("`env -S` runs a command split from a string".into());
                    }
                    let takes_value = matches!(t, "-u" | "-C") && !t.contains('=');
                    i += 1 + usize::from(takes_value);
                } else {
                    break; // the wrapped command
                }
            }
        }

        // Wrappers: the rest of the words are a command.
        if let Some((_, value_flags)) = WRAPPERS.iter().find(|(n, _)| *n == name) {
            let mut i = 0;
            let mut positionals_to_skip = usize::from(name == "timeout"); // the duration
            while i < args.len() {
                let w = &args[i];
                if name == "env" && w.text.contains('=') && !w.text.starts_with('-') {
                    if is_risky_git_env(w.text.split('=').next().unwrap_or("")) {
                        self.git_env_risky = true;
                    }
                    i += 1;
                } else if flag(w) {
                    // `--opt=value` carries its value; a bare option that
                    // takes one consumes the next word.
                    let takes_value =
                        !w.text.contains('=') && value_flags.contains(&w.text.as_str());
                    i += 1 + usize::from(takes_value);
                } else if positionals_to_skip > 0 {
                    positionals_to_skip -= 1;
                    i += 1;
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
        if OPAQUE_WRAPPERS.contains(&name) {
            if !args.is_empty()
                && args.iter().all(|w| matches!(w.text.as_str(), "--version" | "--help" | "-h" | "-V"))
            {
                return None;
            }
            return Some(format!("`{name}` runs a command this check cannot parse"));
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
                .then(|| format!("`{name} -i` rewrites files in place"))
                .or_else(|| {
                    (name == "sed" && args.iter().any(|w| w.text == "-f" || w.text == "--file" || w.text.starts_with("--file=")))
                        .then(|| "`sed -f` runs a script file this check cannot read".into())
                })
                .or_else(|| {
                    (name == "sed" && args.iter().filter(|w| !flag(w) || w.text == "-e").any(|w| sed_script_writes(&w.text)))
                        .then(|| "a `sed` script writes files or runs commands (`w`/`e`)".into())
                }),
            "awk" | "gawk" | "mawk" | "nawk" => {
                let inplace = args.iter().enumerate().any(|(i, w)| {
                    w.text == "-iinplace"
                        || w.text == "--include=inplace"
                        || (w.text == "-i" && args.get(i + 1).is_some_and(|n| n.text == "inplace"))
                });
                if inplace {
                    return Some(format!("`{name} -i inplace` rewrites files in place"));
                }
                if args.iter().any(|w| w.text == "-f" || w.text == "--file" || w.text.starts_with("--file=")) {
                    return Some(format!("`{name} -f` runs a program file this check cannot read"));
                }
                // A program that runs commands or writes files.
                args.iter()
                    .filter(|w| !flag(w))
                    .any(|w| {
                        let t = w.text.as_str();
                        t.contains("system(")
                            || t.contains("| \"")
                            || t.contains("|\"")
                            || t.contains("> \"")
                            || t.contains(">\"")
                            || t.contains(">>")
                            || t.contains("print >")
                            || t.contains("printf >")
                            || t.contains("close(")
                    })
                    .then(|| format!("`{name}` program runs commands or writes files"))
            }
            "gzip" | "bzip2" | "xz" | "zstd" | "lz4" | "lzma" | "gunzip" | "bunzip2" | "unxz" | "unzstd" | "unlz4" | "unlzma" => {
                let keeps = args.iter().any(|w| {
                    matches!(w.text.as_str(), "--keep" | "--stdout" | "--to-stdout" | "--test" | "--list" | "--version")
                        || (flag(w) && !w.text.starts_with("--") && w.text[1..].chars().any(|c| matches!(c, 'k' | 'c' | 't' | 'l' | 'V')))
                });
                (!keeps).then(|| format!("`{name}` replaces its input file with the compressed one"))
            }
            "unzip" => has_flag('o', "--overwrite")
                .then(|| "`unzip -o` overwrites existing files".into()),
            "tar" | "bsdtar" => {
                let extracts = args.iter().any(|w| {
                    w.text == "--extract"
                        || w.text == "--get"
                        || (flag(w) && !w.text.starts_with("--") && w.text[1..].contains('x'))
                        // old style without a dash: `tar xzf a.tgz`
                        || (!flag(w) && args.first().is_some_and(|f| std::ptr::eq(f, w)) && w.text.starts_with('x'))
                });
                (extracts && !args.iter().any(|w| w.text == "--keep-old-files" || w.text == "-k"))
                    .then(|| "`tar` extraction overwrites existing files".into())
            }
            "zip" => has_flag('m', "--move")
                .then(|| "`zip -m` deletes the files it adds".into()),
            "patch" => (!args.iter().any(|w| w.text == "--dry-run" || w.text == "-C" || w.text == "--check"))
                .then(|| "`patch` rewrites files".into()),
            "7z" | "7za" | "7zr" | "7zz" => args
                .iter()
                .any(|w| w.text == "-y" || w.text.starts_with("-ao"))
                .then(|| "`7z` is set to overwrite files".into()),
            "rename" | "prename" | "mmv" => Some(format!("`{name}` renames or overwrites files")),
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
            // Combined short flags ending in the output flag: `-sSLo out`.
            let cluster_o = t.len() > 2
                && t.starts_with('-')
                && !t.starts_with("--")
                && shorts.iter().any(|sh| t.ends_with(&sh[1..]));
            if name == "curl" && t.len() > 2 && t.starts_with('-') && !t.starts_with("--") && t[1..].contains('O') {
                return Some("`curl -O` writes a file named by the server".into());
            }
            let value: Option<Word> = if shorts.contains(&t) || cluster_o || longs.contains(&t) {
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
        if let Some(w) = args.iter().find(|w| {
            matches!(w.text.as_str(), "-fprint" | "-fprint0" | "-fprintf" | "-fls")
        }) {
            return Some(format!("`find {}` writes a file", w.text));
        }
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
        if self.git_env_risky {
            return Some("an environment variable set for git can make it run a program".into());
        }
        // Skip global options (`-C dir`, `-c k=v`, `--git-dir=...`).
        let mut i = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            if t == "-c" && args.get(i + 1).is_some_and(|v| sets_command_key(&v.text)) {
                return Some("a `git -c` setting runs a shell command".into());
            }
            // `--config-env=key=VAR` / `--config-env key=VAR`: the value is an
            // environment variable this check cannot see, so a key that can run
            // a command (or pull in a file that sets one) asks.
            let config_env = if let Some(v) = t.strip_prefix("--config-env=") {
                Some(v)
            } else if t == "--config-env" {
                args.get(i + 1).map(|v| v.text.as_str())
            } else {
                None
            };
            if let Some(kv) = config_env {
                let key = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
                if sets_command_key(kv) || key.starts_with("alias.") {
                    return Some("a `git --config-env` setting can run a shell command".into());
                }
            }
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
            "config" => {
                let reads = any(&|t| {
                    matches!(t, "--get" | "--get-all" | "--get-regexp" | "--list" | "-l" | "--unset" | "--unset-all")
                });
                (!reads && rest.iter().any(|w| sets_command_key(&w.text) || w.text.starts_with('!')))
                    .then_some("`git config` stores a setting that runs a command")
            }
            "apply" => Some("`git apply` rewrites files"),
            "mv" => any(&|t| t == "-f" || t == "--force")
                .then_some("`git mv -f` overwrites an existing path"),
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

/// Whether a `key` or `key=value` names a git setting that runs a command
/// or pulls in a file that can (`include.path`, `includeIf.*.path`; a `!` alias, `core.fsmonitor`, hooks path, pager, editor, ssh command,
/// `diff.external`, `filter.*`, `credential.helper`).
fn sets_command_key(setting: &str) -> bool {
    let s = setting.to_ascii_lowercase();
    let key = s.split('=').next().unwrap_or("");
    (key.starts_with("alias.") && s.contains("=!"))
        // An included file can set any of the keys below.
        || key == "include.path"
        || key.starts_with("includeif.") && key.ends_with(".path")
        || [
            "core.fsmonitor",
            "core.hookspath",
            "core.pager",
            "core.editor",
            "core.sshcommand",
            "sequence.editor",
            "diff.external",
            "credential.helper",
            "gpg.program",
        ]
        .contains(&key)
        || key.starts_with("filter.")
        || key.starts_with("diff.") && key.ends_with(".textconv")
        || key.starts_with("merge.") && key.ends_with(".driver")
}

/// Whether a sed script contains a `w` (write to file) or `e` (execute)
/// command or `s///` flag. Conservative: it may ask about a harmless script.
fn sed_script_writes(script: &str) -> bool {
    let t = script.trim_start();
    t.starts_with("w ")
        || t.starts_with("w/")
        || t.starts_with("e ")
        || ["/w ", "/w/", ";w ", "\nw ", "}w ", "/e;", "/ge", "/ew", "/eg", ";e ", "/pw ", "/gw "]
            .iter()
            .any(|p| script.contains(p))
        || t.ends_with("/e")
        || t.ends_with("/w")
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

    #[test]
    fn program_text_that_writes_or_runs_commands_asks() {
        let d = scratch();
        std::fs::write(d.join("out.bin"), "x").unwrap();
        for c in [
            "awk 'BEGIN{system(\"rm x\")}'",
            "awk '{print > \"out.txt\"}' in",
            "awk '{print | \"sh\"}' in",
            "sed 's/a/b/w out.txt' in",
            "sed -e 'w out.txt' in",
            "sed -n '/x/w out.txt' in",
            "sed 's/a/b/e' in",
            "git -c alias.x=!rm status",
            "git config alias.x '!rm -rf .'",
            "git config --global alias.ci '!sh run'",
            "curl -sSLo out.bin http://x",
            "curl -fsSLo out.bin http://x",
            "curl -sSLO http://x/a",
            "gzip file",
            "xz file",
            "bzip2 file",
            "unzip -o a.zip",
            "tar xzf a.tgz",
            "tar -xf a.tar",
            "tar --extract -f a.tar",
            "patch -p1",
            "rename s/a/b/ f",
            "zip -m a.zip f",
        ] {
            assert!(classify(c, &d).is_some(), "should ask: {c:?}");
        }
        for c in [
            "gzip -k file",
            "gzip -c file",
            "xz --keep file",
            "tar -tf a.tar",
            "tar czf a.tgz dir",
            "unzip a.zip",
            "awk '{print $1}' in",
            "sed -n 1,3p in",
            "sed 's/a/b/' in",
            "git config user.name x",
            "curl -sSL http://x",
        ] {
            assert!(classify(c, &d).is_none(), "should not ask: {c:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wrapper_options_are_skipped_per_wrapper() {
        let d = scratch();
        for c in [
            "time -p rm x",
            "sudo -E rm x",
            "sudo -u root rm x",
            "timeout -s KILL 5 rm x",
            "timeout --signal=KILL 5 rm x",
            "timeout -k 2 5 rm x",
            "timeout 5 rm x",
            "nice -n 5 rm x",
            "env -u FOO rm x",
            "xargs -n 1 rm",
            "parallel rm ::: a b",
            "flock lock rm x",
            "script -qc 'rm x' /dev/null",
            "su -c 'rm x' root",
            "runuser -u me -- rm x",
            "unshare -r rm x",
            "fakeroot rm x",
            "proot rm x",
            "firejail rm x",
            "bwrap --bind / / rm x",
            "chrt 10 rm x",
            "taskset 1 rm x",
            "strace -o log rm x",
            "ltrace rm x",
            "valgrind rm x",
            "systemd-run --user rm x",
            "at now",
            "batch",
            "gdb -batch -ex 'shell rm x'",
            "julia -e 'rm(\"x\")'",
            "pwsh -c 'rm x'",
            "tclsh x.tcl",
            "Rscript -e 'unlink(1)'",
            "osascript -e 'x'",
            "expect -c 'spawn rm x'",
            "awk -f prog.awk in",
            "sed -f prog.sed in",
            "git config core.fsmonitor 'rm x'",
            "git config core.hooksPath /tmp/h",
            "git config sequence.editor 'sh x'",
            "git config diff.external x",
            "git config filter.x.clean 'sh x'",
            "git config credential.helper '!sh x'",
            "git -c core.pager='sh x' log",
            "env -S 'rm x'",
            "X='a b' rm x",
            "GIT_PAGER='sh x' git log",
            "PAGER='sh x' git log",
            "env GIT_EXTERNAL_DIFF=/tmp/x git diff",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.pager GIT_CONFIG_VALUE_0='sh x' git log",
            "env GIT_CONFIG_PARAMETERS=\"'core.pager=sh x'\" git log",
            "GIT_SSH_COMMAND='sh x' git fetch",
            "GIT_ASKPASS=/tmp/x git push origin main",
            "GIT_EDITOR='sh x' git commit",
            "sudo env GIT_PAGER=x git log",
            "export GIT_PAGER='sh x'; git log",
            "GIT_PAGER='sh x'; git log",
            "GIT_PAGER='sh x' && git log",
            "declare -x GIT_EXTERNAL_DIFF=/tmp/x; git diff",
            "typeset -x PAGER=x; git log",
            "readonly GIT_SSH_COMMAND='sh x'; git fetch",
            "export GIT_CONFIG_GLOBAL=/tmp/evil; git log",
            "export GIT_CONFIG_SYSTEM=/tmp/evil; git log",
            "export GIT_PROXY_COMMAND=/tmp/x; git fetch",
            "env -S'rm x'",
            "env --split-string='rm x'",
            "env -i -S 'rm x'",
            "git config include.path /tmp/evil.cfg",
            "git config includeIf.gitdir:/x/.path /tmp/evil.cfg",
            "git -c include.path=/tmp/evil.cfg log",
            "git --config-env=core.pager=EVIL log",
            "git --config-env core.pager=EVIL log",
            "git --config-env=include.path=EVIL log",
            "git --config-env=alias.x=EVIL x",
            "git apply p.diff",
            "git mv -f a b",
            "gunzip -f a.gz",
            "gunzip a.gz",
            "7z x -y a.7z",
            "find . -fprint out.txt",
            "find . -fprintf out.txt '%p'",
        ] {
            assert!(classify(c, &d).is_some(), "should ask: {c:?}");
        }
        for c in [
            "time -p ls",
            "sudo -E ls",
            "timeout -s KILL 5 ls",
            "timeout 5 sleep 1",
            "patch --dry-run -p1",
            "git config --get core.pager",
            "git config --get include.path",
            "git --config-env=user.name=NAME log",
            "command -v rm",
            "env ls -S",
            "export FOO=1; git log",
            "export GIT_AUTHOR_NAME=me; git log",
            "GIT_PAGER=cat; ls",
            "env FOO=1 ls -S",
            "PAGER=cat ls",
            "GIT_AUTHOR_NAME=me git log",
            "env GIT_AUTHOR_NAME=me git log",
            "command -V rm",
            "command -pv rm",
            "env FOO=1 ls",
            "git config user.email a@b",
            "git mv a b",
            "gunzip -k a.gz",
            "7z l a.7z",
            "find . -name x -print",
        ] {
            assert!(classify(c, &d).is_none(), "should not ask: {c:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
