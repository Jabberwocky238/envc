//! envc -- profile based environment variable manager for bash.
//!
//! Everything lives in this one file, by request.
//!
//! What it manages:
//!
//! ```text
//! ~/.envc/
//! ├── profiles/
//! │   ├── work/.env      <- the file a profile loads
//! │   └── dev/.env
//! └── stack               <- the restore stack (text, git-diff style)
//! ```
//!
//! A process can never modify its parent shell's environment, so the commands
//! that change the environment (`activate`, `deactivate`, `autoload`) print
//! shell code on stdout instead of exporting anything themselves:
//!
//! ```text
//! eval "$(envc activate work)"
//! ```
//!
//! `envc enable` installs a small `envc` shell function into ~/.bashrc that
//! performs that `eval` for you, plus a hook that re-applies the selected
//! profile in every new shell.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

// ===========================================================================
// Constants
// ===========================================================================

const PROFILE_ENV_FILE: &str = ".env";
const STACK_FORMAT_VERSION: u32 = 1;

const BEGIN_MARKER: &str = "# >>> envc initialize >>>";
const END_MARKER: &str = "# <<< envc initialize <<<";

const AFTER_HELP: &str = "\
HOW ACTIVATION WORKS:
  A child process cannot change its parent shell, so activate/deactivate print
  shell code on stdout. Run them through the wrapper that `envc enable`
  installs, or call them explicitly:

      eval \"$(envc activate work)\"
      eval \"$(envc deactivate)\"

  Every variable a profile overrides is written to the restore stack
  (~/.envc/stack) as a diff: the '-' lines are what the shell had before, the
  '+' lines are what the profile installed. deactivate replays the '-' lines,
  so variables the profile introduced are unset again and the ones it replaced
  get their old values back.

SHELL INTEGRATION:
  `envc init` detects the login shell from $SHELL and injects the startup hook
  into that shell's rc file -- ~/.zshrc on macOS, ~/.bashrc on Linux. Re-running
  it is safe: an existing hook is detected and left alone.

ENVIRONMENT:
  ENVC_HOME     envc directory                (default: ~/.envc)
  ENVC_RC       rc file to patch, overriding the detection
                (default: ~/.zshrc or ~/.bashrc)
  NO_COLOR      disable colored output
";

// ===========================================================================
// Errors
// ===========================================================================

/// An error worth showing the user. clap handles bad invocations itself, so
/// this only covers runtime failures: missing profiles, unreadable files, ...
#[derive(Debug)]
struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

type Result<T> = std::result::Result<T, Error>;

/// Attach context to an `io::Error`, so messages read like
/// `cannot read ~/.envc/profiles/work/.env: No such file or directory`.
trait IoContext<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| err(format!("{what}: {e}")))
    }
}

// ===========================================================================
// Paths
// ===========================================================================

fn home_dir() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err(err("HOME is not set; cannot locate the envc directory")),
    }
}

/// Root of everything envc manages. `ENVC_HOME` overrides it (tests use this).
fn envc_home() -> Result<PathBuf> {
    match std::env::var_os("ENVC_HOME") {
        Some(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Ok(home_dir()?.join(".envc")),
    }
}

fn profiles_dir() -> Result<PathBuf> {
    Ok(envc_home()?.join("profiles"))
}

fn profile_dir(name: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(name))
}

/// `~/.envc/profiles/<name>/.env` -- the file a profile loads.
fn profile_env_file(name: &str) -> Result<PathBuf> {
    Ok(profile_dir(name)?.join(PROFILE_ENV_FILE))
}

/// `~/.envc/stack` -- the restore stack.
fn stack_path() -> Result<PathBuf> {
    Ok(envc_home()?.join("stack"))
}

/// Which shell's startup file we are dealing with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    Bash,
    Zsh,
}

impl ShellKind {
    fn name(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::Zsh => "zsh",
        }
    }

    /// The startup file an interactive shell of this kind reads.
    fn rc_name(self) -> &'static str {
        match self {
            ShellKind::Bash => ".bashrc",
            ShellKind::Zsh => ".zshrc",
        }
    }

    /// What a fresh install on this OS should target: macOS has shipped zsh as
    /// the login shell since Catalina, Linux is almost always bash.
    fn default_for_os() -> Self {
        if cfg!(target_os = "macos") {
            ShellKind::Zsh
        } else {
            ShellKind::Bash
        }
    }
}

/// Guess the login shell from `$SHELL`, falling back to the OS default. The rc
/// file belongs to the login shell, not to whatever shell runs `envc`.
fn detect_shell() -> ShellKind {
    if let Some(shell) = std::env::var_os("SHELL") {
        let name = Path::new(&shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("zsh") {
            return ShellKind::Zsh;
        }
        if name.contains("bash") {
            return ShellKind::Bash;
        }
    }
    ShellKind::default_for_os()
}

/// Where the detection came from, for the `init` report.
fn detection_source() -> String {
    match std::env::var("SHELL") {
        Ok(s) if !s.is_empty() => format!("$SHELL={s}"),
        _ => format!("the {} default", std::env::consts::OS),
    }
}

/// The rc file `init` / `enable` / `disable` patch.
///
/// `ENVC_RC` overrides the detection; `ENVC_BASHRC` is accepted as a legacy
/// alias for it.
fn rc_file() -> Result<PathBuf> {
    for var in ["ENVC_RC", "ENVC_BASHRC"] {
        if let Some(p) = std::env::var_os(var) {
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    Ok(home_dir()?.join(detect_shell().rc_name()))
}

/// Profile names become directory names, so keep them boring: no separators,
/// no leading dot, no `..`.
fn validate_profile_name(name: &str) -> Result<()> {
    let bad = |why: &str| Err(err(format!("invalid profile name {name:?}: {why}")));

    if name.is_empty() {
        return bad("must not be empty");
    }
    if name.len() > 64 {
        return bad("must be at most 64 characters");
    }
    if name.starts_with('.') {
        return bad("must not start with a dot");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return bad("only letters, digits, '_', '-' and '.' are allowed");
    }
    Ok(())
}

/// Show paths under $HOME as `~/...` to keep listings short.
fn tildify(path: &Path) -> String {
    if let Ok(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

// ===========================================================================
// Shell code emission
// ===========================================================================

/// Characters safe to leave unquoted in a bash word. Deliberately conservative:
/// `$`, backtick, `*`, `?`, `~`, `!`, `#`, `&`, `;`, spaces and quotes are all
/// absent, so they always end up quoted.
fn is_safe_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '%' | '+' | ',' | '-' | '.' | '/' | ':' | '=' | '@' | '_' | '^')
}

/// Quote a value for bash. Single quotes stop all expansion; embedded single
/// quotes are emitted with the `'\''` dance.
fn quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.chars().all(is_safe_unquoted) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `export KEY='value'`
fn export_line(key: &str, value: &str) -> String {
    format!("export {key}={}", quote(value))
}

/// `unset KEY`
fn unset_line(key: &str) -> String {
    format!("unset {key}")
}

/// A valid sh variable name: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Write shell code to stdout. Write errors are ignored: a closed pipe
/// (`envc list | head`) is not worth reporting.
fn emit(lines: &[String]) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
}

// ===========================================================================
// .env parsing
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct Assignment {
    key: String,
    value: String,
}

/// Read and parse a profile's `.env`.
fn parse_env_file(path: &Path) -> Result<Vec<Assignment>> {
    let content = std::fs::read_to_string(path).ctx(format!("cannot read {}", tildify(path)))?;
    parse_env(&content, &tildify(path))
}

/// Parse `.env` content. `origin` only appears in error messages.
///
/// Expansion resolves against the process environment plus any key assigned
/// earlier in the same file, which is what people expect from a `.env`:
///
/// ```text
/// ROOT=/opt/app
/// BIN=$ROOT/bin     # -> /opt/app/bin
/// ```
fn parse_env(content: &str, origin: &str) -> Result<Vec<Assignment>> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    let mut out = Vec::new();

    for (idx, raw) in content.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fail = |msg: &str| err(format!("{origin}:{lineno}: {msg}"));

        let Some((raw_key, raw_value)) = strip_export(line).split_once('=') else {
            return Err(fail("expected `KEY=value`"));
        };

        let key = raw_key.trim();
        if !is_identifier(key) {
            return Err(fail(&format!("{key:?} is not a valid variable name")));
        }

        let value = parse_value(raw_value.trim(), &env)
            .map_err(|e| fail(&format!("in the value of {key}: {e}")))?;

        env.insert(key.to_string(), value.clone());
        out.push(Assignment {
            key: key.to_string(),
            value,
        });
    }

    Ok(out)
}

/// Drop a leading `export` keyword (only when followed by whitespace).
fn strip_export(line: &str) -> &str {
    match line.strip_prefix("export") {
        Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
        _ => line,
    }
}

fn parse_value(raw: &str, env: &HashMap<String, String>) -> std::result::Result<String, String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    // Unquoted whitespace is held back until we know it is not trailing, so
    // `KEY=a   ` loses its spaces while `KEY="a "` keeps them.
    let mut pending = String::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        let c = chars[i];

        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }

        match c {
            '\'' if !in_double => {
                flush(&mut out, &mut pending);
                in_single = true;
                i += 1;
            }
            '"' if !in_double => {
                flush(&mut out, &mut pending);
                in_double = true;
                i += 1;
            }
            '\'' | '"' => {
                flush(&mut out, &mut pending);
                in_double = false;
                i += 1;
            }
            '\\' => {
                i += 1;
                let next = *chars.get(i).ok_or("trailing backslash")?;
                let expanded = if in_double {
                    match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other, // \" \\ \$ \` and anything else: literal
                    }
                } else {
                    next // outside quotes a backslash escapes anything
                };
                flush(&mut out, &mut pending);
                out.push(expanded);
                i += 1;
            }
            '$' => {
                let (value, next_i) = expand(&chars, i, env)?;
                flush(&mut out, &mut pending);
                out.push_str(&value);
                i = next_i;
            }
            '#' if !in_double && starts_comment(&chars, i) => break,
            c if c.is_whitespace() && !in_double => {
                pending.push(c);
                i += 1;
            }
            _ => {
                flush(&mut out, &mut pending);
                out.push(c);
                i += 1;
            }
        }
    }

    Ok(out)
}

fn flush(out: &mut String, pending: &mut String) {
    out.push_str(pending);
    pending.clear();
}

/// `KEY=a # note` has a comment; `KEY=a#note` does not.
fn starts_comment(chars: &[char], i: usize) -> bool {
    i == 0 || chars[i - 1].is_whitespace()
}

/// Expand `$VAR`, `${VAR}` or `${VAR:-default}` starting at `chars[start] == '$'`.
/// Returns the replacement and the index just past the expression.
fn expand(
    chars: &[char],
    start: usize,
    env: &HashMap<String, String>,
) -> std::result::Result<(String, usize), String> {
    let mut i = start + 1;
    let Some(&first) = chars.get(i) else {
        return Ok(("$".to_string(), i)); // a lone trailing `$`
    };

    if first != '{' {
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Ok(("$".to_string(), i)); // `$1`, `$.`, ... not ours to expand
        }
        let mut name = String::new();
        while let Some(&c) = chars.get(i) {
            if c.is_ascii_alphanumeric() || c == '_' {
                name.push(c);
                i += 1;
            } else {
                break;
            }
        }
        return Ok((env.get(&name).cloned().unwrap_or_default(), i));
    }

    // `${...}`
    i += 1;
    let mut name = String::new();
    let mut default: Option<String> = None;

    loop {
        let Some(&c) = chars.get(i) else {
            return Err("unterminated `${`".to_string());
        };
        if c == '}' {
            i += 1;
            break;
        }
        if c == ':' && chars.get(i + 1) == Some(&'-') {
            i += 2;
            let (text, next_i) = read_default(chars, i)?;
            default = Some(text);
            i = next_i;
            break;
        }
        if c == '-' && default.is_none() {
            i += 1;
            let (text, next_i) = read_default(chars, i)?;
            default = Some(text);
            i = next_i;
            break;
        }
        name.push(c);
        i += 1;
    }

    if !is_identifier(&name) {
        return Err(format!("invalid variable name in `${{{name}}}`"));
    }

    let current = env.get(&name).cloned().unwrap_or_default();
    match default {
        // `${VAR:-x}` and `${VAR-x}` behave the same here: the default is used
        // whenever the variable is unset or empty. The difference between the
        // two forms only matters in a shell that tracks "unset" separately.
        Some(d) if current.is_empty() => Ok((expand_default(&d, env)?, i)),
        _ => Ok((current, i)),
    }
}

fn expand_default(raw: &str, env: &HashMap<String, String>) -> std::result::Result<String, String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let (value, next_i) = expand(&chars, i, env)?;
            out.push_str(&value);
            i = next_i;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Read everything up to the closing `}` of a `${VAR:-...}` default, tracking
/// nested braces so `${A:-${B}}` works.
fn read_default(chars: &[char], start: usize) -> std::result::Result<(String, usize), String> {
    let mut text = String::new();
    let mut i = start;
    let mut depth = 0usize;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err("unterminated `${`".to_string());
        };
        match c {
            '{' => {
                depth += 1;
                text.push(c);
            }
            '}' if depth == 0 => {
                i += 1;
                break;
            }
            '}' => {
                depth -= 1;
                text.push(c);
            }
            _ => text.push(c),
        }
        i += 1;
    }
    Ok((text, i))
}

// ===========================================================================
// The restore stack (~/.envc/stack)
// ===========================================================================
//
// A plain text file in the shape of a diff. Each variable the active profile
// touched gets a '-' line (what the shell had before) and a '+' line (what the
// profile installed). `deactivate` replays the '-' side.
//
//     # envc restore stack
//     # version 1
//     # profile work
//     --- before envc
//     +++ after `work`
//     -EDITOR=nano
//     +EDITOR=vim
//     -TOKEN
//     +TOKEN=abc
//
// A '-' or '+' line without `=value` means "not set", which is what makes
// `unset` the correct rollback for variables a profile introduced.

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    /// What the variable held before the override. `None` = it was not set.
    prev: Option<String>,
    /// What the profile installed.
    now: Option<String>,
}

/// Records what `activate` overrode so `deactivate` can undo it.
///
/// The entries always describe the environment *before any envc profile was
/// applied* -- not merely the previously active profile. Activating a second
/// profile peels the first one off and re-reads the base values, so
/// A -> B -> deactivate lands back on the shell you started with, not on A.
#[derive(Debug, Clone)]
struct Stack {
    profile: String,
    entries: Vec<Entry>,
}

impl Stack {
    fn new(profile: &str, entries: Vec<Entry>) -> Self {
        Stack {
            profile: profile.to_string(),
            entries,
        }
    }

    /// `Ok(None)` when nothing is active.
    fn load() -> Result<Option<Stack>> {
        let path = stack_path()?;
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(err(format!("cannot read {}: {e}", tildify(&path)))),
        };
        if content.trim().is_empty() {
            return Ok(None);
        }
        parse_stack(&content, &tildify(&path)).map(Some)
    }

    fn save(&self) -> Result<()> {
        let dir = envc_home()?;
        std::fs::create_dir_all(&dir).ctx(format!("cannot create {}", tildify(&dir)))?;
        let path = stack_path()?;
        std::fs::write(&path, self.render())
            .ctx(format!("cannot write {}", tildify(&path)))
    }

    /// Delete the stack file. A missing file is not an error.
    fn remove() -> Result<()> {
        let path = stack_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err(format!("cannot remove {}: {e}", tildify(&path)))),
        }
    }

    fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("# envc restore stack -- written by `envc activate`, replayed by `envc deactivate`.\n");
        s.push_str("#\n");
        s.push_str("# A '-' line is what the shell held before the profile was applied, the\n");
        s.push_str("# '+' line under it is what the profile installed. A line with no `=value`\n");
        s.push_str("# means the variable was not set at all, so undoing it means `unset`.\n");
        s.push_str("#\n");
        s.push_str(&format!("# format {STACK_FORMAT_VERSION}\n"));
        s.push_str(&format!("# active-profile {}\n", self.profile));
        s.push_str("--- before envc\n");
        s.push_str(&format!("+++ after `{}`\n", self.profile));
        for e in &self.entries {
            s.push_str(&format!("-{}\n", render_side(&e.key, e.prev.as_deref())));
            s.push_str(&format!("+{}\n", render_side(&e.key, e.now.as_deref())));
        }
        s
    }

    /// Shell lines that undo every override, newest first.
    fn restore_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .map(|e| match &e.prev {
                Some(v) => export_line(&e.key, v),
                None => unset_line(&e.key),
            })
            .collect()
    }

    /// `current` with this stack's overrides peeled off -- the environment as it
    /// was before this profile was activated. Values captured from here stay
    /// correct when one profile replaces another.
    fn base_env(&self, current: &HashMap<String, String>) -> HashMap<String, String> {
        let mut base = current.clone();
        for entry in &self.entries {
            match &entry.prev {
                Some(v) => base.insert(entry.key.clone(), v.clone()),
                None => base.remove(&entry.key),
            };
        }
        base
    }

    /// Build stack entries for `assignments` against a base environment.
    fn entries_from(base: &HashMap<String, String>, assignments: &[Assignment]) -> Vec<Entry> {
        assignments
            .iter()
            .map(|a| Entry {
                key: a.key.clone(),
                prev: base.get(&a.key).cloned(),
                now: Some(a.value.clone()),
            })
            .collect()
    }
}

fn render_side(key: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("{key}={}", escape_value(v)),
        None => key.to_string(),
    }
}

/// Keep one entry per line: values may legitimately contain newlines.
fn escape_value(value: &str) -> String {
    let mut s = String::new();
    for c in value.chars() {
        match c {
            '\\' => s.push_str(r"\\"),
            '\n' => s.push_str(r"\n"),
            '\r' => s.push_str(r"\r"),
            _ => s.push(c),
        }
    }
    s
}

fn unescape_value(value: &str) -> String {
    let mut s = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            s.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => s.push('\n'),
            Some('r') => s.push('\r'),
            Some('\\') => s.push('\\'),
            // An unknown escape is kept verbatim rather than silently eaten.
            Some(other) => {
                s.push('\\');
                s.push(other);
            }
            None => s.push('\\'),
        }
    }
    s
}

fn parse_stack(content: &str, origin: &str) -> Result<Stack> {
    let mut version = STACK_FORMAT_VERSION;
    let mut profile: Option<String> = None;
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending: Option<(String, Option<String>)> = None;

    for (idx, raw) in content.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let fail = |msg: String| err(format!("{origin}:{lineno}: {msg}"));

        if let Some(rest) = line.strip_prefix('#') {
            let rest = rest.trim();
            if let Some(v) = rest.strip_prefix("format ") {
                version = v
                    .trim()
                    .parse()
                    .map_err(|_| fail(format!("bad format number {v:?}")))?;
            } else if let Some(p) = rest.strip_prefix("active-profile ") {
                profile = Some(p.trim().to_string());
            }
            // Any other comment is prose and carries no meaning.
            continue;
        }
        // The `---` / `+++` file headers are decoration, not entries.
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }

        if let Some(rest) = line.strip_prefix('-') {
            if pending.is_some() {
                return Err(fail("a '-' line was not followed by its '+' line".into()));
            }
            pending = Some(split_side(rest, &fail)?);
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            let (key, now) = split_side(rest, &fail)?;
            let Some((prev_key, prev)) = pending.take() else {
                return Err(fail("a '+' line has no matching '-' line".into()));
            };
            if prev_key != key {
                return Err(fail(format!(
                    "'+' line for {key:?} does not match the preceding '-' line for {prev_key:?}"
                )));
            }
            entries.push(Entry { key, prev, now });
            continue;
        }
        return Err(fail(format!("unrecognised line: {line:?}")));
    }

    if let Some((key, _)) = pending {
        return Err(err(format!("{origin}: '{key}' has a '-' line but no '+' line")));
    }
    if version != STACK_FORMAT_VERSION {
        return Err(err(format!(
            "{origin}: written with stack format {version}, this build understands {STACK_FORMAT_VERSION}"
        )));
    }
    let Some(profile) = profile else {
        return Err(err(format!(
            "{origin}: missing a `# active-profile <name>` line"
        )));
    };
    Ok(Stack { profile, entries })
}

fn split_side<F: Fn(String) -> Error>(
    side: &str,
    fail: &F,
) -> Result<(String, Option<String>)> {
    let (key, value) = match side.split_once('=') {
        Some((k, v)) => (k, Some(unescape_value(v))),
        None => (side, None),
    };
    if !is_identifier(key) {
        return Err(fail(format!("{key:?} is not a valid variable name")));
    }
    Ok((key.to_string(), value))
}

// ===========================================================================
// ~/.bashrc hook
// ===========================================================================

/// The block `envc enable` appends to ~/.bashrc. It defines an `envc` shell
/// function that evals the output of activate/deactivate, then re-applies the
/// selected profile via `envc autoload`.
fn hook_block() -> String {
    format!(
        "\n{BEGIN_MARKER}\n\
         # Managed by `envc enable` / `envc disable` -- do not edit this block by hand.\n\
         if command -v envc >/dev/null 2>&1; then\n\
         \x20   envc() {{\n\
         \x20       case \"${{1:-}}\" in\n\
         \x20           activate|use|deactivate|de|unuse)\n\
         \x20               # ENVC_WRAPPED tells the binary its output is being eval'd.\n\
         \x20               eval \"$(ENVC_WRAPPED=1 command envc \"$@\")\"\n\
         \x20               ;;\n\
         \x20           *)\n\
         \x20               command envc \"$@\"\n\
         \x20               ;;\n\
         \x20       esac\n\
         \x20   }}\n\
         \x20   eval \"$(command envc autoload)\"\n\
         fi\n\
         {END_MARKER}\n"
    )
}

fn read_if_exists(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(err(format!("cannot read {}: {e}", tildify(path)))),
    }
}

fn rc_is_enabled() -> Result<bool> {
    let content = read_if_exists(&rc_file()?)?;
    Ok(content.contains(BEGIN_MARKER))
}

#[derive(Debug, PartialEq, Eq)]
enum EnableOutcome {
    /// The hook was appended for the first time.
    Installed,
    /// An existing hook was replaced (e.g. after upgrading envc).
    Refreshed,
}

fn rc_enable() -> Result<EnableOutcome> {
    let path = rc_file()?;
    let content = read_if_exists(&path)?;
    let (mut updated, had_block) = strip_block(&content);

    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&hook_block());
    std::fs::write(&path, updated).ctx(format!("cannot write {}", tildify(&path)))?;

    Ok(if had_block {
        EnableOutcome::Refreshed
    } else {
        EnableOutcome::Installed
    })
}

/// Returns `true` when a block was actually removed.
fn rc_disable() -> Result<bool> {
    let path = rc_file()?;
    let content = read_if_exists(&path)?;
    let (stripped, had_block) = strip_block(&content);
    if had_block {
        std::fs::write(&path, stripped).ctx(format!("cannot write {}", tildify(&path)))?;
    }
    Ok(had_block)
}

/// Remove everything from the begin marker to the end marker inclusive, plus
/// the blank separator line in front of it.
fn strip_block(content: &str) -> (String, bool) {
    let mut out = String::new();
    let mut inside = false;
    let mut found = false;

    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == BEGIN_MARKER {
            inside = true;
            found = true;
            // Drop the blank line inserted before the block, so repeated
            // enable/disable cycles do not accumulate empty lines.
            if out.ends_with("\n\n") {
                out.pop();
            }
            continue;
        }
        if trimmed == END_MARKER {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }

    // Preserve the original "no trailing newline" state of the file.
    if !content.is_empty() && !content.ends_with('\n') {
        out.pop();
    }
    (out, found)
}

// ===========================================================================
// Commands
// ===========================================================================

fn cmd_create(name: &str, force: bool) -> Result<()> {
    validate_profile_name(name)?;

    let dir = profile_dir(name)?;
    let file = profile_env_file(name)?;

    if file.exists() && !force {
        return Err(err(format!(
            "profile '{name}' already exists ({}); pass --force to overwrite it",
            tildify(&file)
        )));
    }

    std::fs::create_dir_all(&dir).ctx(format!("cannot create {}", tildify(&dir)))?;
    std::fs::write(&file, template(name)).ctx(format!("cannot write {}", tildify(&file)))?;

    println!("created profile '{name}'");
    println!("  env file: {}", tildify(&file));
    println!("  edit it, then run: envc activate {name}");

    if !rc_is_enabled()? {
        println!();
        println!("note: startup loading is off, so new shells will not load this profile.");
        println!("      run `envc init` to turn it on.");
    }
    Ok(())
}

/// A blank starting point. envc never edits a profile's `.env` again -- the
/// file belongs to the user, this only gives it somewhere to start.
fn template(name: &str) -> String {
    format!("# envc profile: {name}\n# KEY=value, one per line. Edit this file however you like.\n")
}

fn cmd_list() -> Result<()> {
    let dir = profiles_dir()?;
    let active = Stack::load()?.map(|s| s.profile);
    let c = Palette::new();

    let mut names = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.ctx(format!("cannot read {}", tildify(&dir)))?;
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if let Some(name) = entry.file_name().to_str() {
                        names.push(name.to_string());
                    }
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(err(format!("cannot read {}: {e}", tildify(&dir)))),
    }
    names.sort();

    if names.is_empty() {
        println!("no profiles yet.");
        println!("create one with: envc create <name>");
        println!();
        print_startup_line(&c, active.as_deref())?;
        return Ok(());
    }

    // name -> (var count or "!", optional parse error)
    let mut rows = Vec::new();
    let mut width = "PROFILE".len();
    for name in &names {
        let file = profile_env_file(name)?;
        let (count, error) = match parse_env_file(&file) {
            Ok(a) => (a.len().to_string(), None),
            Err(e) => ("!".to_string(), Some(e.to_string())),
        };
        width = width.max(name.chars().count());
        rows.push((name.clone(), count, error, tildify(&file)));
    }

    // Two leading spaces: one for the active marker, one as a separator.
    println!(
        "  {:<width$}  {:>4}  {}",
        c.bold("PROFILE"),
        "VARS",
        c.bold("ENV FILE"),
        width = width
    );
    for (name, count, error, file) in &rows {
        let is_active = active.as_deref() == Some(name.as_str());
        let marker = if is_active { c.green("*") } else { " ".to_string() };
        let label = format!("{name:<width$}", width = width);
        let label = if is_active { c.bold(&label) } else { label };
        let file = if error.is_some() { c.red(file) } else { c.dim(file) };
        println!("{marker} {label}  {count:>4}  {file}");
    }

    for (name, _, error, _) in &rows {
        if let Some(e) = error {
            eprintln!("envc: profile '{name}' could not be parsed: {e}");
        }
    }
    if let Some(active) = &active {
        if !names.contains(active) {
            eprintln!(
                "envc: active profile '{active}' has no directory under {}",
                tildify(&dir)
            );
        }
    }

    println!();
    print_startup_line(&c, active.as_deref())?;
    Ok(())
}

fn print_startup_line(c: &Palette, active: Option<&str>) -> Result<()> {
    let rc = rc_file()?;
    let enabled = rc_is_enabled()?;
    let state = if enabled {
        c.green("enabled")
    } else {
        c.yellow("disabled")
    };
    let suffix = if enabled {
        format!(" ({})", tildify(&rc))
    } else {
        String::new()
    };
    println!("startup loading: {state}{suffix}");

    match active {
        Some(name) => println!("selected profile: {}", c.bold(name)),
        None => println!("selected profile: (none)"),
    }
    Ok(())
}

fn cmd_delete(name: &str, force: bool, quiet: bool) -> Result<()> {
    validate_profile_name(name)?;

    let dir = profile_dir(name)?;
    if !dir.is_dir() {
        return Err(err(format!("no such profile: {name}")));
    }

    let active = Stack::load()?.map(|s| s.profile);
    let is_active = active.as_deref() == Some(name);

    if is_active && !force {
        return Err(err(format!(
            "profile '{name}' is currently active; run `envc deactivate` first, or pass --force"
        )));
    }

    std::fs::remove_dir_all(&dir).ctx(format!("cannot remove {}", tildify(&dir)))?;

    if is_active {
        Stack::remove()?;
        notify(quiet, format!("cleared the restore stack for the deleted profile '{name}'"));
        if !quiet {
            eprintln!("envc: warning: this shell still holds the variables it exported;");
            eprintln!("envc:          run `envc deactivate` to drop them, or open a new shell.");
        }
    }

    println!("deleted profile '{name}' ({})", tildify(&dir));
    Ok(())
}

fn cmd_activate(name: &str, quiet: bool) -> Result<()> {
    validate_profile_name(name)?;

    let file = profile_env_file(name)?;
    if !file.is_file() {
        return Err(err(format!(
            "no such profile: '{name}' (expected {})",
            tildify(&file)
        )));
    }
    let assignments = parse_env_file(&file)?;

    let current: HashMap<String, String> = std::env::vars().collect();
    let previous = Stack::load()?;

    let mut lines = Vec::new();
    // Peel the previous profile off first, so the values we remember are the
    // ones the shell had before *any* envc profile was applied.
    let base = match &previous {
        Some(old) => {
            lines.extend(old.restore_lines());
            old.base_env(&current)
        }
        None => current.clone(),
    };

    let entries = Stack::entries_from(&base, &assignments);
    for a in &assignments {
        lines.push(export_line(&a.key, &a.value));
    }
    lines.push(format!("export ENVC_ACTIVE={}", quote(name)));
    emit(&lines);

    Stack::new(name, entries).save()?;

    let what = match &previous {
        None => format!("activated '{name}'"),
        Some(old) if old.profile == name => format!("reloaded '{name}'"),
        Some(old) => format!("switched '{}' -> '{name}'", old.profile),
    };
    notify(quiet, format!("{what} ({} vars)", assignments.len()));
    print_activation_hint(quiet, &format!("envc activate {name}"));
    Ok(())
}

fn cmd_deactivate(quiet: bool) -> Result<()> {
    match Stack::load()? {
        Some(stack) => {
            let mut lines = stack.restore_lines();
            lines.push("unset ENVC_ACTIVE".to_string());
            emit(&lines);
            Stack::remove()?;

            notify(
                quiet,
                format!(
                    "deactivated '{}' ({} vars restored)",
                    stack.profile,
                    stack.entries.len()
                ),
            );
            print_activation_hint(quiet, "envc deactivate");
        }
        None => {
            if std::env::var_os("ENVC_ACTIVE").is_some() {
                emit(&["unset ENVC_ACTIVE".to_string()]);
                notify(quiet, "nothing on the restore stack; cleared ENVC_ACTIVE only");
            } else {
                notify(quiet, "no profile is active in this shell");
            }
        }
    }
    Ok(())
}

/// Re-apply the selected profile in a fresh shell. Called by the ~/.bashrc hook.
///
/// Unlike `activate` there is no previous stack to peel off: a new shell starts
/// from the login environment, which is exactly the base we want to remember.
fn cmd_autoload() -> Result<()> {
    let Some(stack) = Stack::load()? else {
        return Ok(());
    };

    let file = profile_env_file(&stack.profile)?;
    if !file.is_file() {
        eprintln!(
            "envc: profile '{}' is selected but {} is missing; skipping autoload",
            stack.profile,
            tildify(&file)
        );
        return Ok(());
    }

    let assignments = parse_env_file(&file)?;
    let current: HashMap<String, String> = std::env::vars().collect();

    let mut lines = Vec::new();
    for a in &assignments {
        lines.push(export_line(&a.key, &a.value));
    }
    lines.push(format!("export ENVC_ACTIVE={}", quote(&stack.profile)));
    emit(&lines);

    Stack::new(&stack.profile, Stack::entries_from(&current, &assignments)).save()?;
    Ok(())
}

/// First-time setup: work out which startup file the login shell reads, then
/// inject the hook into it unless it is already there.
fn cmd_init(quiet: bool) -> Result<()> {
    let rc = rc_file()?;
    let shell = detect_shell();
    let already = rc_is_enabled()?;

    println!("shell:   {} (from {})", shell.name(), detection_source());
    println!("rc file: {}", tildify(&rc));

    if already {
        println!("hook:    already installed, nothing to do");
    } else {
        rc_enable()?;
        println!("hook:    installed");
    }

    warn_if_not_on_path();

    if !already {
        println!();
        println!("open a new shell, or run `source {}` to use it right now.", tildify(&rc));
    }
    if !quiet {
        println!();
        println!("next:");
        println!("    envc create work      # write a profile");
        println!("    envc activate work    # apply it to this shell");
    }
    Ok(())
}

fn cmd_enable() -> Result<()> {
    let rc = rc_file()?;
    match rc_enable()? {
        EnableOutcome::Installed => {
            println!("startup loading enabled");
            println!("  installed the hook into {}", tildify(&rc));
        }
        EnableOutcome::Refreshed => {
            println!("startup loading enabled");
            println!("  refreshed the hook in {}", tildify(&rc));
        }
    }
    println!(
        "  open a new shell (or run: source {}) to pick it up",
        tildify(&rc)
    );

    warn_if_not_on_path();
    Ok(())
}

/// The hook is useless if new shells cannot find the binary, and that is an easy
/// mistake to make after a plain `cargo build`.
fn warn_if_not_on_path() {
    if binary_on_path() {
        return;
    }
    eprintln!();
    eprintln!("envc: warning: `envc` is not on PATH, so the hook cannot fire in new shells.");
    eprintln!("envc:          this binary is {}", current_exe());
    eprintln!("envc:          install it, for example:");
    eprintln!(
        "envc:              install -Dm755 {} {}",
        current_exe(),
        suggested_install_path()
    );
}

/// `~/.local/bin` on Linux, `/usr/local/bin` on macOS (where the former is
/// rarely on PATH).
fn suggested_install_path() -> String {
    let dir = if cfg!(target_os = "macos") {
        PathBuf::from("/usr/local/bin")
    } else {
        home_dir()
            .map(|h| h.join(".local/bin"))
            .unwrap_or_else(|_| PathBuf::from("/usr/local/bin"))
    };
    dir.join("envc").display().to_string()
}

fn cmd_disable(quiet: bool) -> Result<()> {
    let rc = rc_file()?;
    if rc_disable()? {
        println!("startup loading disabled");
        println!("  removed the hook from {}", tildify(&rc));
        if !quiet {
            println!("  the `envc` shell function stays defined until you open a new shell.");
            println!("  run `envc init` to turn startup loading back on.");
        }
    } else if !quiet {
        println!(
            "startup loading was already disabled ({} has no hook)",
            tildify(&rc)
        );
    }
    Ok(())
}

fn cmd_stack() -> Result<()> {
    let path = stack_path()?;
    let c = Palette::new();
    match Stack::load()? {
        Some(stack) => {
            println!(
                "{} '{}' is active; {} in the restore stack",
                c.bold("envc:"),
                c.bold(&stack.profile),
                stack.entries.len()
            );
            println!("{}", tildify(&path));
            println!();
            print!("{}", stack.render());
        }
        None => {
            println!("no profile is active; {} does not exist", tildify(&path));
        }
    }
    Ok(())
}

fn cmd_status() -> Result<()> {
    let c = Palette::new();
    let home = envc_home()?;
    let rc = rc_file()?;
    let stack = Stack::load()?;
    let shell_active = std::env::var("ENVC_ACTIVE").ok();

    println!("{} {}", c.bold("envc"), env!("CARGO_PKG_VERSION"));
    println!("  {:<17}{}", "home", tildify(&home));

    let state = if rc_is_enabled()? {
        c.green("enabled")
    } else {
        c.yellow("disabled")
    };
    println!("  {:<17}{} ({})", "startup loading", state, tildify(&rc));

    match &stack {
        Some(s) => println!(
            "  {:<17}{} ({} vars, restore stack at {})",
            "selected profile",
            c.bold(&s.profile),
            s.entries.len(),
            tildify(&stack_path()?)
        ),
        None => println!("  {:<17}{}", "selected profile", c.dim("(none)")),
    }

    match &shell_active {
        Some(name) => println!("  {:<17}{}", "this shell", c.bold(name)),
        None => println!("  {:<17}(no profile applied)", "this shell"),
    }

    if let (Some(sel), Some(cur)) = (stack.as_ref().map(|s| s.profile.clone()), &shell_active) {
        if &sel != cur {
            eprintln!();
            eprintln!("envc: note: this shell has '{cur}' applied but the selected profile is '{sel}'.");
            eprintln!("envc:       run `envc activate {sel}` here, or `envc deactivate` to clear this shell.");
        }
    }

    let profiles = profiles_dir()?;
    let count = std::fs::read_dir(&profiles)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    println!("  {:<17}{} in {}", "profiles", count, tildify(&profiles));
    Ok(())
}

// ===========================================================================
// Output helpers
// ===========================================================================

fn notify(quiet: bool, msg: impl AsRef<str>) {
    if !quiet {
        eprintln!("envc: {}", msg.as_ref());
    }
}

/// Is the shell code being captured (and will therefore be eval'd), or is it
/// going straight to the terminal where it does nothing useful?
fn hint_wanted() -> bool {
    io::stdout().is_terminal() && std::env::var("ENVC_WRAPPED").as_deref() != Ok("1")
}

/// Printed when shell code was generated but nothing is going to eval it.
fn print_activation_hint(quiet: bool, command: &str) {
    if quiet || !hint_wanted() {
        return;
    }
    eprintln!();
    eprintln!("envc: this shell has NOT been changed yet -- the lines above must be evaluated:");
    eprintln!("envc:     eval \"$({command})\"");
    eprintln!("envc: run `envc enable` once to install a wrapper so `{command}` just works.");
}

fn binary_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("envc").is_file())
}

fn current_exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "envc".to_string())
}

/// ANSI colors, enabled only on a terminal and unless NO_COLOR is set.
struct Palette {
    on: bool,
}

impl Palette {
    fn new() -> Self {
        Palette {
            on: io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
}

// ===========================================================================
// Command line
// ===========================================================================

#[derive(Parser, Debug)]
#[command(
    name = "envc",
    version,
    about = "Profile based environment variable manager for bash",
    after_help = AFTER_HELP,
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Suppress informational messages
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a profile template at ~/.envc/profiles/<name>/.env
    Create {
        /// Profile name (letters, digits, '_', '-', '.')
        name: String,

        /// Overwrite the profile if it already exists
        #[arg(short, long)]
        force: bool,
    },

    /// List profiles, marking the active one
    #[command(visible_alias = "ls")]
    List,

    /// Delete a profile
    #[command(visible_alias = "rm")]
    Delete {
        /// Profile to delete
        name: String,

        /// Delete even when the profile is currently active
        #[arg(short, long)]
        force: bool,
    },

    /// Override this shell's environment with a profile
    #[command(visible_alias = "use")]
    Activate {
        /// Profile to apply
        name: String,
    },

    /// Undo the active profile, restoring the previous values
    #[command(visible_aliases = ["de", "unuse"])]
    Deactivate,

    /// Set up shell integration: detect the login shell's rc file and inject the
    /// startup hook into it (safe to re-run)
    Init,

    /// Install the startup hook into the rc file so new shells load the profile
    Enable,

    /// Remove the startup hook from ~/.bashrc
    Disable,

    /// Show the restore stack that `deactivate` replays
    Stack,

    /// Show what is enabled and active right now
    Status,

    /// Internal: emit shell code for the ~/.bashrc hook
    #[command(hide = true)]
    Autoload,
}

fn run(cli: Cli) -> Result<()> {
    let quiet = cli.quiet;
    match cli.command {
        Command::Create { name, force } => cmd_create(&name, force),
        Command::List => cmd_list(),
        Command::Delete { name, force } => cmd_delete(&name, force, quiet),
        Command::Activate { name } => cmd_activate(&name, quiet),
        Command::Deactivate => cmd_deactivate(quiet),
        Command::Init => cmd_init(quiet),
        Command::Enable => cmd_enable(),
        Command::Disable => cmd_disable(quiet),
        Command::Stack => cmd_stack(),
        Command::Status => cmd_status(),
        Command::Autoload => cmd_autoload(),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("envc: {e}");
            ExitCode::FAILURE
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- profile names ------------------------------------------------------

    #[test]
    fn accepts_reasonable_profile_names() {
        for name in ["work", "client-a", "rust_1.80", "a"] {
            assert!(validate_profile_name(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn rejects_dangerous_profile_names() {
        for name in ["", "..", "../evil", "/abs", "a/b", ".hidden", "a b", "a$b"] {
            assert!(
                validate_profile_name(name).is_err(),
                "{name:?} should be rejected"
            );
        }
    }

    // -- quoting ------------------------------------------------------------

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote("/usr/bin:/bin"), "/usr/bin:/bin");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("hello world"), "'hello world'");
        assert_eq!(quote("a$b"), "'a$b'");
        assert_eq!(quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    /// The real contract: whatever `quote` produces, bash must read back as the
    /// original string. Verified against an actual bash, not by eyeballing.
    #[test]
    fn quoted_values_survive_a_real_shell() {
        let values = [
            "plain",
            "",
            "with spaces",
            "it's",
            r#"mixed "quotes" and 'apostrophes'"#,
            "$HOME",
            "`whoami`",
            "a;rm -rf /",
            "a\nb\tc",
            "*?[]{}~!#&|<>",
            "日本語 の 値",
            "back\\slash",
        ];
        for value in values {
            let script = format!("printf %s {}", quote(value));
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("bash should be available");
            assert!(out.status.success(), "bash failed for {value:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                value,
                "round trip failed for {value:?} (script: {script})"
            );
        }
    }

    #[test]
    fn recognises_identifiers() {
        assert!(is_identifier("PATH"));
        assert!(is_identifier("_x1"));
        assert!(!is_identifier("1x"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier(""));
    }

    // -- .env parsing -------------------------------------------------------

    fn parsed(content: &str) -> Vec<Assignment> {
        parse_env(content, "test.env").expect("should parse")
    }

    fn value_of(content: &str, key: &str) -> String {
        parsed(content)
            .into_iter()
            .find(|a| a.key == key)
            .unwrap_or_else(|| panic!("{key} missing"))
            .value
    }

    #[test]
    fn parses_plain_assignments() {
        let a = parsed("A=1\nB=two\n");
        assert_eq!(a[0], Assignment { key: "A".into(), value: "1".into() });
        assert_eq!(a[1], Assignment { key: "B".into(), value: "two".into() });
    }

    #[test]
    fn skips_blanks_and_comments() {
        assert_eq!(parsed("\n# a comment\n   \nA=1\n   # indented comment\n").len(), 1);
    }

    #[test]
    fn export_prefix_is_optional() {
        assert_eq!(value_of("export A=1\n", "A"), "1");
        assert_eq!(value_of("export\tA=1\n", "A"), "1");
        // `exportFOO=1` declares a variable named exportFOO, not an export.
        assert_eq!(value_of("exportFOO=1\n", "exportFOO"), "1");
    }

    #[test]
    fn handles_quoting() {
        assert_eq!(value_of("A=\"a b\"\n", "A"), "a b");
        assert_eq!(value_of("A='a b'\n", "A"), "a b");
        assert_eq!(value_of("A=\"a \"\n", "A"), "a ");
        assert_eq!(value_of("A='$HOME'\n", "A"), "$HOME");
    }

    #[test]
    fn trims_unquoted_trailing_space_but_keeps_quoted() {
        assert_eq!(value_of("A=abc   \n", "A"), "abc");
        assert_eq!(value_of("A=\"abc   \"\n", "A"), "abc   ");
        assert_eq!(value_of("A=abc \"  \"\n", "A"), "abc   ");
    }

    #[test]
    fn inline_comments_only_after_whitespace() {
        assert_eq!(value_of("A=abc # note\n", "A"), "abc");
        assert_eq!(value_of("A=abc#notacomment\n", "A"), "abc#notacomment");
        assert_eq!(value_of("A=\"a # b\"\n", "A"), "a # b");
    }

    #[test]
    fn expands_keys_defined_earlier_in_the_same_file() {
        assert_eq!(value_of("ROOT=/opt\nBIN=$ROOT/bin\n", "BIN"), "/opt/bin");
        assert_eq!(value_of("ROOT=/opt\nBIN=${ROOT}/bin\n", "BIN"), "/opt/bin");
    }

    #[test]
    fn expands_the_process_environment() {
        let home = std::env::var("HOME").expect("HOME should be set on linux");
        assert_eq!(value_of("A=$HOME!\n", "A"), format!("{home}!"));
        assert_eq!(value_of("A=${HOME}\n", "A"), home);
    }

    #[test]
    fn unset_variables_expand_to_empty_or_their_default() {
        assert_eq!(value_of("A=$ENVC_UNSET_XYZ\n", "A"), "");
        assert_eq!(value_of("A=${ENVC_UNSET_XYZ:-fallback}\n", "A"), "fallback");
        assert_eq!(value_of("A=${ENVC_UNSET_XYZ-fallback}\n", "A"), "fallback");
        assert_eq!(value_of("A=${ENVC_UNSET_XYZ:-$HOME}\n", "A"), std::env::var("HOME").unwrap());
        assert_eq!(value_of("A=${ENVC_UNSET_XYZ:-${ENVC_UNSET_ABC:-deep}}\n", "A"), "deep");
    }

    #[test]
    fn a_dollar_that_starts_no_expansion_stays_literal() {
        assert_eq!(value_of("A=price$\n", "A"), "price$");
        assert_eq!(value_of("A='$'\n", "A"), "$");
        assert_eq!(value_of("A=\\$HOME\n", "A"), "$HOME");
        assert_eq!(value_of("A=$1\n", "A"), "$1");
    }

    #[test]
    fn reports_the_offending_line() {
        let e = parse_env("A=1\nnot an assignment\n", "x.env").unwrap_err();
        assert!(e.to_string().contains("x.env:2"), "{e}");
    }

    #[test]
    fn rejects_bad_keys_in_an_env_file() {
        assert!(parse_env("1BAD=1\n", "x.env").is_err());
        assert!(parse_env("A-B=1\n", "x.env").is_err());
    }

    // -- restore stack ------------------------------------------------------

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn sample_stack() -> Stack {
        Stack::new(
            "work",
            vec![
                Entry { key: "EDITOR".into(), prev: Some("nano".into()), now: Some("vim".into()) },
                Entry { key: "TOKEN".into(), prev: None, now: Some("abc".into()) },
            ],
        )
    }

    #[test]
    fn renders_a_diff_shaped_file() {
        assert_eq!(
            sample_stack().render(),
            "\
# envc restore stack -- written by `envc activate`, replayed by `envc deactivate`.
#
# A '-' line is what the shell held before the profile was applied, the
# '+' line under it is what the profile installed. A line with no `=value`
# means the variable was not set at all, so undoing it means `unset`.
#
# format 1
# active-profile work
--- before envc
+++ after `work`
-EDITOR=nano
+EDITOR=vim
-TOKEN
+TOKEN=abc
"
        );
    }

    #[test]
    fn stack_round_trips_through_its_text_format() {
        for stack in [
            sample_stack(),
            Stack::new("empty", vec![]),
            Stack::new(
                "odd",
                vec![
                    Entry { key: "A".into(), prev: None, now: None },
                    Entry { key: "B".into(), prev: Some(String::new()), now: Some("x=y=z".into()) },
                    Entry { key: "C".into(), prev: Some("a\nb".into()), now: Some("c\\d".into()) },
                    Entry { key: "D".into(), prev: Some("+minus".into()), now: Some("-plus".into()) },
                ],
            ),
        ] {
            let parsed = parse_stack(&stack.render(), "stack").expect("should parse");
            assert_eq!(parsed.profile, stack.profile);
            assert_eq!(parsed.entries, stack.entries);
        }
    }

    #[test]
    fn restores_previous_values_and_unsets_new_ones() {
        // Reverse order: TOKEN is unset before EDITOR is restored.
        assert_eq!(
            sample_stack().restore_lines(),
            vec!["unset TOKEN".to_string(), "export EDITOR=nano".to_string()]
        );
    }

    #[test]
    fn an_empty_previous_value_restores_to_empty_not_unset() {
        let stack = Stack::new(
            "p",
            vec![Entry { key: "A".into(), prev: Some(String::new()), now: Some("x".into()) }],
        );
        assert_eq!(stack.restore_lines(), vec!["export A=''".to_string()]);
    }

    #[test]
    fn base_env_peels_the_stack_off() {
        let stack = Stack::new(
            "work",
            vec![
                Entry { key: "EDITOR".into(), prev: Some("nano".into()), now: Some("vim".into()) },
                Entry { key: "TOKEN".into(), prev: None, now: Some("abc".into()) },
                Entry { key: "PATH".into(), prev: Some("/usr/bin".into()), now: Some("/opt/bin".into()) },
            ],
        );
        let current = env(&[
            ("EDITOR", "vim"),
            ("TOKEN", "abc"),
            ("PATH", "/opt/bin"),
            ("HOME", "/root"),
        ]);

        let base = stack.base_env(&current);
        assert_eq!(base.get("EDITOR").map(String::as_str), Some("nano"));
        assert_eq!(base.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(base.get("TOKEN"), None, "a var the profile introduced is removed");
        assert_eq!(base.get("HOME").map(String::as_str), Some("/root"), "untouched vars survive");
    }

    /// Switching profiles must remember the *original* values, not the ones the
    /// first profile installed -- otherwise A -> B -> deactivate lands on A.
    #[test]
    fn switching_profiles_keeps_the_original_base() {
        let base = env(&[("EDITOR", "nano")]);
        let a_assignments = [Assignment { key: "EDITOR".into(), value: "vim".into() }];

        // activate A: EDITOR -> vim
        let a = Stack::new("A", Stack::entries_from(&base, &a_assignments));
        assert_eq!(a.entries[0].prev.as_deref(), Some("nano"));

        // activate B: peel A off, then capture against the peeled base
        let after_a = env(&[("EDITOR", "vim")]);
        let peeled = a.base_env(&after_a);
        assert_eq!(peeled.get("EDITOR").map(String::as_str), Some("nano"));

        let b_assignments = [Assignment { key: "EDITOR".into(), value: "emacs".into() }];
        let b = Stack::new("B", Stack::entries_from(&peeled, &b_assignments));
        assert_eq!(b.entries[0].prev.as_deref(), Some("nano"), "B must roll back to the base");
        assert_eq!(b.restore_lines(), vec!["export EDITOR=nano".to_string()]);
    }

    #[test]
    fn rejects_a_corrupt_stack() {
        assert!(parse_stack("total nonsense\n", "stack").is_err(), "unknown line");
        assert!(parse_stack("-A=1\n", "stack").is_err(), "dangling '-' line");
        assert!(parse_stack("+A=1\n", "stack").is_err(), "orphan '+' line");
        assert!(parse_stack("-A=1\n+B=2\n", "stack").is_err(), "mismatched pair");
        assert!(parse_stack("-1BAD=1\n+1BAD=2\n", "stack").is_err(), "bad key");
        assert!(parse_stack("# format 99\n-A=1\n+A=2\n", "stack").is_err(), "future format");
        assert!(parse_stack("-A=1\n+A=2\n", "stack").is_err(), "no profile header");
    }

    #[test]
    fn escape_round_trips() {
        for value in ["plain", "", "a\nb", "back\\slash", "trailing\\", "a\rb", "混\n合"] {
            assert_eq!(unescape_value(&escape_value(value)), value, "{value:?}");
        }
    }

    // -- bashrc hook --------------------------------------------------------

    #[test]
    fn strip_block_removes_the_block_and_its_blank_separator() {
        let content = format!("before\n{}after\n", hook_block());
        let (out, found) = strip_block(&content);
        assert!(found);
        assert_eq!(out, "before\nafter\n");
    }

    #[test]
    fn strip_block_on_a_file_without_the_hook_is_a_no_op() {
        let (out, found) = strip_block("nothing here\n");
        assert!(!found);
        assert_eq!(out, "nothing here\n");
    }

    #[test]
    fn enable_is_idempotent() {
        let original = "export PATH=$HOME/bin:$PATH\nalias ll='ls -l'\n";

        let once = format!("{original}{}", hook_block());
        let (stripped, had) = strip_block(&once);
        assert!(had);
        assert_eq!(stripped, original, "removing the hook restores the original file");

        let twice = format!("{stripped}{}", hook_block());
        assert_eq!(once, twice, "a second enable must not stack up blocks");
    }

    #[test]
    fn hook_defines_the_wrapper_and_calls_autoload() {
        let script = hook_block();
        assert!(script.contains("envc()"));
        assert!(script.contains("envc autoload"));
        assert!(script.contains("ENVC_WRAPPED=1"));
    }

    // -- clap wiring --------------------------------------------------------

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn aliases_resolve() {
        let use_cli = Cli::try_parse_from(["envc", "use", "work"]).expect("`use work` parses");
        assert!(matches!(use_cli.command, Command::Activate { .. }));

        for alias in ["de", "unuse"] {
            let cli = Cli::try_parse_from(["envc", alias]).expect("alias parses");
            assert!(matches!(cli.command, Command::Deactivate), "{alias}");
        }
        let ls = Cli::try_parse_from(["envc", "ls"]).expect("`ls` parses");
        assert!(matches!(ls.command, Command::List));
        let rm = Cli::try_parse_from(["envc", "rm", "work"]).expect("`rm work` parses");
        assert!(matches!(rm.command, Command::Delete { .. }));
    }

    // -- shell detection ----------------------------------------------------

    fn cloned_without_shell<T>(f: impl FnOnce() -> T) -> T {
        // The tests run in one process, so `$SHELL` has to be restored.
        let saved = std::env::var_os("SHELL");
        std::env::remove_var("SHELL");
        let out = f();
        match saved {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        out
    }

    #[test]
    fn falls_back_to_the_os_default_when_shell_is_unset() {
        let kind = cloned_without_shell(detect_shell);
        assert_eq!(kind, ShellKind::default_for_os());
        // macOS has shipped zsh since Catalina; Linux is bash.
        if cfg!(target_os = "macos") {
            assert_eq!(kind.rc_name(), ".zshrc");
        } else {
            assert_eq!(kind.rc_name(), ".bashrc");
        }
    }

    #[test]
    fn maps_rc_names_to_shells() {
        assert_eq!(ShellKind::Bash.rc_name(), ".bashrc");
        assert_eq!(ShellKind::Zsh.rc_name(), ".zshrc");
        assert_eq!(ShellKind::Bash.name(), "bash");
        assert_eq!(ShellKind::Zsh.name(), "zsh");
    }

    /// The hook is written once and has to work in both shells.
    #[test]
    fn generated_hook_is_valid_posix_shell_syntax() {
        let script = hook_block();
        for shell in ["bash", "zsh", "dash"] {
            let available = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {shell}"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !available {
                continue;
            }
            let out = std::process::Command::new(shell)
                .arg("-n")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("shell should run");
            assert!(
                out.status.success(),
                "{shell} rejected the generated hook:\n{}\n{script}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn quiet_is_accepted_before_and_after_the_subcommand() {
        assert!(Cli::try_parse_from(["envc", "-q", "list"]).unwrap().quiet);
        assert!(Cli::try_parse_from(["envc", "list", "--quiet"]).unwrap().quiet);
        assert!(!Cli::try_parse_from(["envc", "list"]).unwrap().quiet);
    }
}
