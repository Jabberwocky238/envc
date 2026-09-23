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
use std::ffi::OsString;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use anyhow::{anyhow, Context as _};
use clap::{Parser, Subcommand};

// ===========================================================================
// Constants
// ===========================================================================

const PROFILE_ENV_FILE: &str = ".env";
const STACK_FORMAT_VERSION: u32 = 1;
const STARTUP_FORMAT_VERSION: u32 = 1;

/// The variable that records which profile this shell has applied.
const ENVC_ACTIVE_VAR: &str = "ENVC_ACTIVE";

/// The markers that fence off envc's block inside an rc file. POSIX only.
#[cfg(not(windows))]
const BEGIN_MARKER: &str = "# >>> envc initialize >>>";
#[cfg(not(windows))]
const END_MARKER: &str = "# <<< envc initialize <<<";

const AFTER_HELP: &str = "\
TWO HALVES:
  Startup    -- `envc enable <name>` picks the profile new shells load and
                installs the hook; `envc disable` stops it. Kept in
                ~/.envc/startup, so it outlives any single shell.
  This shell -- `envc activate <name>` applies a profile here;
                `envc deactivate` undoes it. Neither changes the next shell.

ACTIVATION:
  A child process cannot change its parent shell, so activate/deactivate print
  shell code on stdout. Run them through the wrapper `envc enable` installs, or:

      eval \"$(envc activate work)\"

  Each override goes into the restore stack (~/.envc/stack) as the old value
  against the new one; deactivate replays the old side.

WINDOWS:
  No rc file is read by both cmd and PowerShell, so `enable` puts the profile's
  variables into the user environment instead: permanent, and visible to every
  program started afterwards. `disable` restores the previous values.

  Activation still only affects the current shell. PowerShell evaluates what is
  printed; cmd has no `eval`, so the code goes to %TEMP%\\envc\\activate.cmd:

      powershell   envc activate work | Invoke-Expression
      cmd          envc activate work && call \"%TEMP%\\envc\\activate.cmd\"

SHELL INTEGRATION:
  `envc init` detects the login shell from $SHELL and injects the hook into
  ~/.bashrc or ~/.zshrc. Re-running is safe.

ENVIRONMENT:
  ENVC_HOME     envc directory                (default: ~/.envc)
  ENVC_RC       rc file to patch, overriding the detection
  ENVC_SHELL    bash | powershell | cmd -- which syntax to emit
  NO_COLOR      disable colored output
";

// ===========================================================================
// Errors
// ===========================================================================

/// Runtime failures only -- clap handles bad invocations itself. `anyhow` gives
/// us the context chain for free, so there is no error type of our own here.
type Result<T> = anyhow::Result<T>;

// ===========================================================================
// The environment, read once
// ===========================================================================

/// Everything envc reads out of the environment, snapshotted on first use.
///
/// A process's environment cannot change under it, so looking the same variable
/// up again for every caller is wasted work -- `Tilde` was reading $HOME once
/// per formatted path, inside `list`'s loop. One snapshot also turns "what does
/// envc read from the environment?" into a single question with a single answer,
/// which is worth having in a program whose whole job is environment variables.
///
/// Two reads stay outside on purpose: command-line arguments are clap's, and
/// the terminal check is not an environment variable at all.
// A few fields only matter on one platform (the login shell, the rc file) --
// this is the whole environment surface, so gating each one would be noise.
#[cfg_attr(windows, allow(dead_code))]
struct Env {
    /// The whole environment, which is the base profile values expand against
    /// and what `activate` records for `deactivate` to replay.
    vars: HashMap<String, String>,

    /// Where `~/.envc` lives. `ENVC_HOME` overrides the default.
    envc_home: Option<PathBuf>,
    /// The home directory, and the names that were looked for, so the error can
    /// name them.
    home: Option<PathBuf>,
    home_keys: &'static [&'static str],
    /// The rc file `init` / `enable` / `disable` patch: `ENVC_RC` (or the legacy
    /// `ENVC_BASHRC`), else the login shell's file under $HOME.
    #[cfg(not(windows))]
    rc: Option<PathBuf>,

    /// `$SHELL`, which says which rc file belongs to the login shell.
    login_shell: Option<String>,
    /// `ENVC_SHELL`, and whether PowerShell left PSModulePath behind. Both feed
    /// the choice of which syntax to emit.
    envc_shell: Option<String>,
    ps_module_path: bool,

    /// `SystemRoot`, to find the built-in PowerShell when PATH does not.
    /// Windows only; nothing on POSIX looks at it.
    #[cfg_attr(not(windows), allow(dead_code))]
    system_root: Option<PathBuf>,
    /// Where the cmd hand-off script goes.
    temp_dir: PathBuf,

    /// `ENVC_ACTIVE` -- the profile this shell has applied.
    active: Option<String>,
    /// `ENVC_WRAPPED` -- set by the shell wrapper while it is capturing output.
    wrapped: bool,
    /// `NO_COLOR` -- and whether stdout is a terminal, which is not an
    /// environment variable but is the other half of the same decision.
    no_color: bool,
    /// `PATH`, for the "is envc on PATH?" check.
    path: Option<OsString>,
}

impl Env {
    fn read() -> Env {
        /// An environment variable, treating empty as unset -- which is what
        /// every caller here wants.
        fn var(key: &str) -> Option<String> {
            std::env::var(key).ok().filter(|v| !v.is_empty())
        }
        fn path(key: &str) -> Option<PathBuf> {
            std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
        }

        // Windows sets USERPROFILE (and, in shells that emulate POSIX, HOME);
        // POSIX shells set HOME. Accepting both keeps `envc` usable from
        // git-bash too.
        let home_keys: &[&str] = if cfg!(windows) {
            &["HOME", "USERPROFILE"]
        } else {
            &["HOME"]
        };
        let home = home_keys.iter().find_map(|k| path(k));
        let login_shell = var("SHELL");
        // Worked out before the literal below, which moves `home` into it.
        let envc_home = path("ENVC_HOME").or_else(|| home.as_ref().map(|h| h.join(".envc")));

        // The rc file is a POSIX idea; Windows has none to point at.
        #[cfg(not(windows))]
        let rc = ["ENVC_RC", "ENVC_BASHRC"]
            .iter()
            .find_map(|k| path(k))
            .or_else(|| {
                // `shell_kind` and not `detect_shell`: the latter reads this
                // snapshot, and reading it from inside its own initialiser
                // would deadlock on the `LazyLock`.
                home.as_ref()
                    .map(|h| h.join(shell_kind(login_shell.as_deref()).rc_name()))
            });

        Env {
            vars: std::env::vars().collect(),
            envc_home,
            home,
            home_keys,
            #[cfg(not(windows))]
            rc,
            login_shell,
            envc_shell: var("ENVC_SHELL"),
            ps_module_path: std::env::var_os("PSModulePath").is_some(),
            system_root: path("SystemRoot"),
            temp_dir: std::env::temp_dir(),
            active: var("ENVC_ACTIVE"),
            wrapped: var("ENVC_WRAPPED").as_deref() == Some("1"),
            // Set-and-empty still counts: that is what the NO_COLOR
            // convention says, so this one does not filter empties.
            no_color: std::env::var_os("NO_COLOR").is_some(),
            path: std::env::var_os("PATH"),
        }
    }

    /// The snapshot. Initialised on first use, once.
    fn get() -> &'static Env {
        static ENV: LazyLock<Env> = LazyLock::new(Env::read);
        &ENV
    }
}

// ===========================================================================
// Paths
// ===========================================================================

/// What to say when a directory cannot be worked out, naming the variables that
/// were looked for.
fn not_set() -> anyhow::Error {
    anyhow!("{} is not set", Env::get().home_keys.join(" / "))
}

/// The home directory. Only the POSIX install hint wants it directly; every
/// other path goes through the snapshot.
#[cfg(not(windows))]
fn home_dir() -> Result<&'static Path> {
    Env::get().home.as_deref().ok_or_else(not_set)
}

/// Root of everything envc manages. `ENVC_HOME` overrides it (tests use this).
fn envc_home() -> Result<&'static Path> {
    Env::get().envc_home.as_deref().ok_or_else(not_set)
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

/// `~/.envc/startup` -- the profile new shells load.
fn startup_path() -> Result<PathBuf> {
    Ok(envc_home()?.join("startup"))
}

/// `~/.envc/user-stack` -- Windows only: what the user-level variables held
/// before `enable` overwrote them, so `disable` can put them back.
///
/// A separate file from the session stack: the two layers have different
/// lifetimes (`deactivate` must not undo what `disable` is for), and they can
/// both be live at once.
#[cfg(windows)]
fn user_stack_path() -> Result<PathBuf> {
    Ok(envc_home()?.join("user-stack"))
}

/// Which shell's startup file we are dealing with. POSIX only: Windows has no
/// rc file to pick.
#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    Bash,
    Zsh,
}

#[cfg(not(windows))]
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

/// Which shell a `$SHELL` value means. Kept pure so the snapshot can call it
/// while it is still being built.
#[cfg(not(windows))]
fn shell_kind(shell: Option<&str>) -> ShellKind {
    if let Some(name) = shell {
        let name = Path::new(name)
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

/// The login shell's kind. The rc file belongs to the login shell, not to
/// whatever shell runs `envc`.
#[cfg(not(windows))]
fn detect_shell() -> ShellKind {
    shell_kind(Env::get().login_shell.as_deref())
}

/// Where the detection came from, for the `init` report.
#[cfg(not(windows))]
fn detection_source() -> String {
    match Env::get().login_shell.as_deref() {
        Some(shell) => format!("$SHELL={shell}"),
        None => format!("the {} default", std::env::consts::OS),
    }
}

/// The rc file `init` / `enable` / `disable` patch.
///
/// `ENVC_RC` overrides the detection; `ENVC_BASHRC` is accepted as a legacy
/// alias for it.
#[cfg(not(windows))]
fn rc_file() -> Result<&'static Path> {
    Env::get().rc.as_deref().ok_or_else(not_set)
}

/// Profile names become directory names, so keep them boring: no separators,
/// no leading dot, no `..`.
fn validate_profile_name(name: &str) -> Result<()> {
    let bad = |why: &str| Err(anyhow!("invalid profile name {name:?}: {why}"));

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
///
/// A `Display` wrapper rather than a function returning `String`, so it can be
/// dropped straight into `format!` / `anyhow!` without allocating a temporary
/// string first. Paths are tildified in loops (see `list`), so that adds up.
struct Tilde<'a>(&'a Path);

impl<'a> From<&'a Path> for Tilde<'a> {
    fn from(path: &'a Path) -> Self {
        Tilde(path)
    }
}

impl fmt::Display for Tilde<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // From the snapshot: this runs once per formatted path, inside
        // `list`'s loop, so it must not be reading anything.
        if let Some(rest) = Env::get()
            .home
            .as_deref()
            .and_then(|home| self.0.strip_prefix(home).ok())
        {
            return if rest.as_os_str().is_empty() {
                f.write_str("~")
            } else {
                write!(f, "~/{}", rest.display())
            };
        }
        write!(f, "{}", self.0.display())
    }
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

/// PowerShell takes the same trick as bash, with a different escape: doubling
/// the quote instead of closing and reopening it.
fn quote_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// cmd is the odd one out: there is no quoting to speak of, only the
/// `set "KEY=VALUE"` form, which ends at the *last* quote on the line. A `%`
/// would be read as a variable reference inside a batch file, so it is doubled.
fn set_cmd(key: &str, value: &str) -> String {
    format!("set \"{key}={}\"", value.replace('%', "%%"))
}

/// Which shell's syntax to emit.
///
/// bash and zsh take identical code, so they share `Posix`. Windows has no
/// `eval`, which is why `Cmd` does not print anything evaluable at all.
// The variant is named after the shell it means; clippy's "ends with the enum
// name" is exactly what is wanted here.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Posix,
    PowerShell,
    Cmd,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Shell::Posix => "bash",
            Shell::PowerShell => "powershell",
            Shell::Cmd => "cmd",
        }
    }

    /// `--shell` / `ENVC_SHELL`, else guess.
    fn resolve(flag: Option<&str>) -> Result<Self> {
        let wanted = flag
            .map(str::to_string)
            .or_else(|| Env::get().envc_shell.clone());

        match wanted.as_deref().map(str::trim) {
            None => Ok(Shell::detect()),
            Some("bash" | "zsh" | "sh" | "posix") => Ok(Shell::Posix),
            Some("powershell" | "pwsh" | "ps") => Ok(Shell::PowerShell),
            Some("cmd" | "cmd.exe" | "bat" | "batch") => Ok(Shell::Cmd),
            Some(other) => Err(anyhow!(
                "unknown shell {other:?}; expected bash, powershell or cmd"
            )),
        }
    }

    /// PowerShell exports `PSModulePath` to every session it starts, including
    /// the ones it hands to a child process; cmd does not. That is the only
    /// signal available from inside the child.
    fn detect() -> Self {
        if cfg!(windows) {
            if Env::get().ps_module_path {
                Shell::PowerShell
            } else {
                Shell::Cmd
            }
        } else {
            Shell::Posix
        }
    }

    fn assign(self, key: &str, value: &str) -> String {
        match self {
            Shell::Posix => format!("export {key}={}", quote(value)),
            Shell::PowerShell => format!("$env:{key}={}", quote_powershell(value)),
            Shell::Cmd => set_cmd(key, value),
        }
    }

    fn unset(self, key: &str) -> String {
        match self {
            Shell::Posix => format!("unset {key}"),
            Shell::PowerShell => {
                format!("Remove-Item Env:{key} -ErrorAction SilentlyContinue")
            }
            Shell::Cmd => set_cmd(key, ""),
        }
    }
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

/// Hand the generated shell code to the caller.
///
/// POSIX shells and PowerShell evaluate what lands on stdout. cmd cannot -- there
/// is no `eval` -- so the code goes into a batch file and the caller is told to
/// `call` it. The file name is fixed, so the idiom can be written down once:
///
///     envc activate work && call "%TEMP%\envc\activate.cmd"
impl Shell {
    /// Hand generated code to whoever asked for it.
    ///
    /// POSIX shells and PowerShell evaluate what lands on stdout. cmd cannot --
    /// there is no `eval` -- so its code goes into a batch file instead and the
    /// caller is told to `call` it. Which of those applies is the shell's
    /// business, which is why the commands just call this.
    fn deliver(self, what: &str, lines: &[String]) -> Result<()> {
        match self {
            Shell::Cmd => self.write_batch(what, lines),
            _ => {
                // Write errors are ignored: a closed pipe (`envc list | head`)
                // is not worth reporting.
                let stdout = io::stdout();
                let mut out = stdout.lock();
                for line in lines {
                    let _ = writeln!(out, "{line}");
                }
                Ok(())
            }
        }
    }

    /// Whether the caller has to evaluate something before this shell changes.
    /// cmd does not: it gets a file to `call`, and `write_batch` says where.
    fn evaluates_stdout(self) -> bool {
        self != Shell::Cmd
    }

    /// The cmd detour: `%TEMP%\envc\<what>.cmd`, which the caller then `call`s.
    fn write_batch(self, what: &str, lines: &[String]) -> Result<()> {
        let path = Env::get().temp_dir.join("envc").join(format!("{what}.cmd"));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", Tilde(dir)))?;
        }
        // cmd wants CRLF, and a batch file starts with echo off or every line
        // it runs gets echoed.
        let mut body = String::from("@echo off\r\n");
        for line in lines {
            body.push_str(line);
            body.push_str("\r\n");
        }
        std::fs::write(&path, body).with_context(|| format!("cannot write {}", Tilde(&path)))?;

        eprintln!("envc: wrote {}", Tilde(&path));
        eprintln!("envc: in cmd, run: call \"{}\"", path.display());
        Ok(())
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
    let content = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", Tilde(path)))?;
    parse_env(&content, Tilde(path))
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
fn parse_env(content: &str, origin: impl fmt::Display) -> Result<Vec<Assignment>> {
    // A copy of the snapshot, because assignments made along the way have to
    // be visible to the lines below them.
    let mut env = Env::get().vars.clone();
    let mut out = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut idx = 0;

    while idx < lines.len() {
        let lineno = idx + 1;
        let line = lines[idx].trim();
        idx += 1;

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fail = |msg: &str| anyhow!("{origin}:{lineno}: {msg}");

        let Some((raw_key, raw_value)) = strip_export(line).split_once('=') else {
            return Err(fail("expected `KEY=value`"));
        };

        let key = raw_key.trim();
        if !is_identifier(key) {
            return Err(fail(&format!("{key:?} is not a valid variable name")));
        }

        // A quoted section is allowed to stay open past the end of the line, so
        // feed it more lines until it closes and let the newlines become part
        // of the value. Only the first line is left-trimmed: what follows is
        // inside the quotes, where whitespace is content. (`lines()` has
        // already dropped any `\r`, and unquoted trailing space is dropped by
        // parse_value itself.)
        let mut raw = raw_value.trim_start().to_string();
        let value = loop {
            match parse_value(&raw, &env) {
                Ok(value) => break Ok(value),
                Err(ValueError::Unterminated(q)) => {
                    if idx >= lines.len() {
                        break Err(fail(&format!(
                            "in the value of {key}: unterminated {q} quote"
                        )));
                    }
                    raw.push('\n');
                    raw.push_str(lines[idx]);
                    idx += 1;
                }
                Err(e) => break Err(fail(&format!("in the value of {key}: {e}"))),
            }
        }?;

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

/// Why a value could not be turned into text.
enum ValueError {
    /// A quoted section was opened and never closed. A quoted value is allowed
    /// to span lines, so the caller can pull in the next line and try again;
    /// at the end of the file this is a real error.
    Unterminated(char),
    Other(String),
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::Unterminated(q) => write!(f, "unterminated {q} quote"),
            ValueError::Other(msg) => f.write_str(msg),
        }
    }
}

/// Turn the text after `=` into the value it stands for.
///
/// Quoting is deliberately conservative about what counts as a quote:
///
///   * `'...'` is literal, `"..."` expands and honours backslash escapes;
///   * a quote only *opens* a section at a boundary -- at the start of the
///     value, or straight after a section closed. Anywhere else it is an
///     ordinary character;
///   * inside a section, the other quote character is ordinary too.
///
/// That is what keeps `MSG=don't stop` and `PATH=C:\a"b` intact. Being
/// permissive here would silently eat the quote, and losing a character the
/// user typed is worse than leaving it visible.
fn parse_value(raw: &str, env: &HashMap<String, String>) -> std::result::Result<String, ValueError> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    // Unquoted whitespace is held back until we know it is not trailing, so
    // `KEY=a   ` loses its spaces while `KEY="a "` keeps them.
    let mut pending = String::new();
    let mut i = 0;
    // Which section we are inside, if any.
    let mut quote: Option<char> = None;
    // Whether the unquoted run so far is empty, i.e. whether a quote here would
    // be starting a section rather than being part of the text.
    let mut at_boundary = true;

    while i < chars.len() {
        let c = chars[i];

        if let Some(q) = quote {
            if c == q {
                quote = None;
                at_boundary = true;
                i += 1;
                continue;
            }
            if q == '\'' {
                // Single quotes are literal: nothing inside is special.
                out.push(c);
                i += 1;
                continue;
            }
            // Inside double quotes, fall through: `$`, `\` and the comment
            // check still apply, but `'` does not close anything.
        }

        match c {
            '\'' | '"' if quote.is_none() && at_boundary => {
                flush(&mut out, &mut pending);
                quote = Some(c);
                i += 1;
            }
            '\'' | '"' => {
                flush(&mut out, &mut pending);
                out.push(c);
                at_boundary = false;
                i += 1;
            }
            '\\' => {
                i += 1;
                let next = *chars
                    .get(i)
                    .ok_or_else(|| ValueError::Other("trailing backslash".to_string()))?;
                let expanded = if quote == Some('"') {
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
                at_boundary = false;
                i += 1;
            }
            '$' => {
                let (value, next_i) = expand(&chars, i, env).map_err(ValueError::Other)?;
                flush(&mut out, &mut pending);
                // An expansion that produced nothing leaves us still at a
                // boundary, so `KEY=$EMPTY'x'` still reads as quoted.
                at_boundary = at_boundary && value.is_empty();
                out.push_str(&value);
                i = next_i;
            }
            '#' if quote != Some('"') && starts_comment(&chars, i) => break,
            c if c.is_whitespace() && quote.is_none() => {
                pending.push(c);
                i += 1;
            }
            _ => {
                flush(&mut out, &mut pending);
                out.push(c);
                at_boundary = false;
                i += 1;
            }
        }
    }

    match quote {
        Some(q) => Err(ValueError::Unterminated(q)),
        None => Ok(out),
    }
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

    fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("# envc restore stack -- `activate` writes it, `deactivate` replays it.\n");
        s.push_str("# '-' is the value before the profile, '+' what it installed.\n");
        s.push_str("# A side with no `=value` means unset, so undoing it is `unset`.\n");
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
    fn restore_lines(&self, shell: Shell) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .map(|e| match &e.prev {
                Some(v) => shell.assign(&e.key, v),
                None => shell.unset(&e.key),
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

/// The profile new shells load, chosen by `envc enable <name>`.
///
/// This is deliberately not the restore stack. The stack is per-shell
/// bookkeeping that `deactivate` replays; this file is the persistent answer to
/// "what should the rc hook apply in a fresh shell?". Only `enable` writes it,
/// so `activate` cannot change what the next shell starts with.
#[derive(Debug, Clone)]
struct Startup {
    profile: String,
}

impl Startup {
    fn render(&self) -> String {
        format!(
            "# envc startup profile -- what new shells load through the rc hook.\n\
             # `envc enable <name>` writes it; `envc disable` keeps it, stopped.\n\
             # format {STARTUP_FORMAT_VERSION}\n\
             {}\n",
            self.profile
        )
    }
}

/// The file is a comment header plus one bare profile name, so a hand-edited
/// file cannot break startup: anything unparseable simply means "no choice".
fn parse_startup(content: &str) -> Option<Startup> {
    let profile = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?;
    Some(Startup {
        profile: profile.to_string(),
    })
}

// ===========================================================================
// Persisted state
// ===========================================================================

/// The three filesystem methods every piece of persisted state needs: read and
/// parse, create the directory and render, and a removal that tolerates a
/// missing file. `Stack` and `Startup` are exactly this shape.
///
/// `$parse` gets the file's contents and its path, and answers `Ok(None)` when
/// the file holds no state at all.
macro_rules! state_file {
    ($ty:ident, $path:ident, $parse:ident) => {
        impl $ty {
            /// `Ok(None)` when the file is absent, empty, or holds no state.
            fn load() -> Result<Option<Self>> {
                let path = $path()?;
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(anyhow!("cannot read {}: {e}", Tilde(&path))),
                };
                $parse(&content, &path)
            }

            fn save(&self) -> Result<()> {
                let dir = envc_home()?;
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("cannot create {}", Tilde(&dir)))?;
                let path = $path()?;
                std::fs::write(&path, self.render())
                    .with_context(|| format!("cannot write {}", Tilde(&path)))
            }

            /// Removing a file that was never written is not an error.
            fn remove() -> Result<()> {
                let path = $path()?;
                match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(anyhow!("cannot remove {}: {e}", Tilde(&path))),
                }
            }
        }
    };
}

state_file!(Stack, stack_path, parse_stack_state);
state_file!(Startup, startup_path, parse_startup_state);

/// Windows: the user-level variables an `enable` overwrote.
///
/// Same diff format and same rendering as the session stack -- only the file
/// differs, because the two layers are independent.
#[cfg(windows)]
struct UserStack(Stack);

#[cfg(windows)]
impl UserStack {
    fn render(&self) -> String {
        self.0.render()
    }
}

#[cfg(windows)]
state_file!(UserStack, user_stack_path, parse_user_stack_state);

#[cfg(windows)]
fn parse_user_stack_state(content: &str, path: &Path) -> Result<Option<UserStack>> {
    Ok(parse_stack_state(content, path)?.map(UserStack))
}

// ===========================================================================
// Windows: permanent user-level variables
// ===========================================================================

/// Run a PowerShell snippet and return its stdout.
///
/// `-NoProfile` so a user's profile cannot change the result, and
/// `-NonInteractive` so it can never sit there waiting for input.
#[cfg(windows)]
fn powershell(script: &str) -> Result<String> {
    // `powershell` is on PATH on any normal Windows, but a trimmed PATH would
    // otherwise fail with something unhelpful, so fall back to where the
    // built-in 5.1 interpreter always lives.
    let mut candidates = vec![PathBuf::from("powershell")];
    if let Some(root) = &Env::get().system_root {
        candidates.push(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    }

    let mut spawn_error = None;
    for exe in &candidates {
        let out = match std::process::Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                spawn_error = Some(format!("{}: {e}", exe.display()));
                continue;
            }
        };
        // It ran, so this is the right interpreter -- a failure now is the
        // script's, not the lookup's.
        if !out.status.success() {
            return Err(anyhow!(
                "powershell failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }

    Err(anyhow!(
        "cannot run powershell ({})",
        spawn_error.unwrap_or_default()
    ))
}

/// Read the current user-level value of each key. `None` means "not set at the
/// user level", which is what makes `disable` able to remove rather than blank.
///
/// One line per key: `KEY=value`, or just `KEY` when it was unset. Values
/// containing newlines cannot be told apart from the line ending -- user-level
/// variables rarely are, and the alternative is a base64 layer for them.
#[cfg(windows)]
fn read_user_vars(keys: &[&str]) -> Result<HashMap<String, Option<String>>> {
    let list = keys
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "{list} | ForEach-Object {{ $v = [Environment]::GetEnvironmentVariable($_,'User'); \
         if ($null -eq $v) {{ $_ }} else {{ \"$_=$v\" }} }}"
    );

    let mut found = HashMap::new();
    for line in powershell(&script)?.lines() {
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (line, None),
        };
        found.insert(key.trim().to_string(), value);
    }
    Ok(found)
}

/// Set (or, for `None`, remove) user-level variables in one PowerShell run.
#[cfg(windows)]
fn write_user_vars(pairs: &[(String, Option<String>)]) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let script = pairs
        .iter()
        .map(|(k, v)| match v {
            Some(v) => format!(
                "[Environment]::SetEnvironmentVariable('{k}',{},'User')",
                quote_powershell(v)
            ),
            None => format!("[Environment]::SetEnvironmentVariable('{k}',$null,'User')"),
        })
        .collect::<Vec<_>>()
        .join("; ");
    powershell(&script)?;
    Ok(())
}

/// An empty restore stack means "nothing is active", so it reads as no state.
fn parse_stack_state(content: &str, path: &Path) -> Result<Option<Stack>> {
    if content.trim().is_empty() {
        return Ok(None);
    }
    parse_stack(content, Tilde(path)).map(Some)
}

fn parse_startup_state(content: &str, _path: &Path) -> Result<Option<Startup>> {
    Ok(parse_startup(content))
}

/// The chosen startup profile, falling back to the restore stack for installs
/// that predate this file -- before, `activate` was what chose it, and starting
/// to load nothing after an upgrade would be a nasty surprise. The adoption is
/// announced once, then persisted so it never happens again.
fn remembered_startup() -> Result<Option<String>> {
    if let Some(startup) = Startup::load()? {
        return Ok(Some(startup.profile));
    }

    let Some(stack) = Stack::load()? else {
        return Ok(None);
    };
    let profile = stack.profile;
    if !profile_env_file(&profile)?.is_file() {
        return Ok(None);
    }

    Startup {
        profile: profile.clone(),
    }
    .save()?;
    eprintln!(
        "envc: '{profile}' is now the startup profile (it was selected before the upgrade; \
         `envc enable <name>` changes it)"
    );
    Ok(Some(profile))
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

fn parse_stack(content: &str, origin: impl fmt::Display) -> Result<Stack> {
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
        let fail = |msg: String| anyhow!("{origin}:{lineno}: {msg}");

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
        return Err(anyhow!("{origin}: '{key}' has a '-' line but no '+' line"));
    }
    if version != STACK_FORMAT_VERSION {
        return Err(anyhow!(
            "{origin}: written with stack format {version}, this build understands {STACK_FORMAT_VERSION}"
        ));
    }
    let Some(profile) = profile else {
        return Err(anyhow!(
            "{origin}: missing a `# active-profile <name>` line"
        ));
    };
    Ok(Stack { profile, entries })
}

fn split_side<F: Fn(String) -> anyhow::Error>(
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
#[cfg(not(windows))]
fn hook_block() -> String {
    format!(
        "\n{BEGIN_MARKER}\n\
         # envc -- managed; do not edit.\n\
         if command -v envc >/dev/null 2>&1; then\n\
         \x20   envc() {{\n\
         \x20       case \"${{1:-}}\" in\n\
         \x20           activate|use|deactivate|de|unuse)\n\
         \x20               # ENVC_WRAPPED: output is eval'd, not printed.\n\
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

#[cfg(not(windows))]
fn read_if_exists(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(anyhow!("cannot read {}: {e}", Tilde(path))),
    }
}

#[cfg(not(windows))]
fn rc_is_enabled() -> Result<bool> {
    let content = read_if_exists(rc_file()?)?;
    Ok(content.contains(BEGIN_MARKER))
}

#[cfg(not(windows))]
#[derive(Debug, PartialEq, Eq)]
enum EnableOutcome {
    /// The hook was appended for the first time.
    Installed,
    /// An existing hook was replaced (e.g. after upgrading envc).
    Refreshed,
}

#[cfg(not(windows))]
fn rc_enable() -> Result<EnableOutcome> {
    let path = rc_file()?;
    let content = read_if_exists(path)?;
    let (mut updated, had_block) = strip_block(&content);

    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&hook_block());
    std::fs::write(path, updated).with_context(|| format!("cannot write {}", Tilde(path)))?;

    Ok(if had_block {
        EnableOutcome::Refreshed
    } else {
        EnableOutcome::Installed
    })
}

/// Returns `true` when a block was actually removed.
#[cfg(not(windows))]
fn rc_disable() -> Result<bool> {
    let path = rc_file()?;
    let content = read_if_exists(path)?;
    let (stripped, had_block) = strip_block(&content);
    if had_block {
        std::fs::write(path, stripped).with_context(|| format!("cannot write {}", Tilde(path)))?;
    }
    Ok(had_block)
}

/// Remove everything from the begin marker to the end marker inclusive, plus
/// the blank separator line in front of it.
#[cfg(not(windows))]
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
        return Err(anyhow!(
            "profile '{name}' already exists ({}); pass --force to overwrite it",
            Tilde(&file)
        ));
    }

    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", Tilde(&dir)))?;
    std::fs::write(&file, template(name)).with_context(|| format!("cannot write {}", Tilde(&file)))?;

    println!("created profile '{name}'");
    println!("  env file: {}", Tilde(&file));
    println!("  edit it, then run: envc activate {name}");

    if !layer().is_on()? {
        println!();
        println!("note: startup loading is off, so new shells will not load this profile.");
        println!("      run `envc enable {name}` to turn it on.");
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
    // The marker tracks the startup choice, not this shell: that is the one
    // that survives, and the one `enable` talks about.
    let startup = Startup::load()?.map(|s| s.profile);
    let c = Palette::new();

    let mut names = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.with_context(|| format!("cannot read {}", Tilde(&dir)))?;
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if let Some(name) = entry.file_name().to_str() {
                        names.push(name.to_string());
                    }
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow!("cannot read {}: {e}", Tilde(&dir))),
    }
    names.sort();

    if names.is_empty() {
        println!("no profiles yet.");
        println!("create one with: envc create <name>");
        println!();
        print_startup_line(&c, startup.as_deref())?;
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
        // The path itself, not a pre-rendered string: it is tildified once, on
        // the way to stdout.
        rows.push((name.clone(), count, error, file));
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
        let is_startup = startup.as_deref() == Some(name.as_str());
        let marker = if is_startup { c.green("*") } else { " ".to_string() };
        let label = format!("{name:<width$}", width = width);
        let label = if is_startup { c.bold(&label) } else { label };
        let file = Tilde(file);
        let file = if error.is_some() { c.red(file) } else { c.dim(file) };
        println!("{marker} {label}  {count:>4}  {file}");
    }

    for (name, _, error, _) in &rows {
        if let Some(e) = error {
            eprintln!("envc: profile '{name}' could not be parsed: {e}");
        }
    }
    if let Some(startup) = &startup {
        if !names.contains(startup) {
            eprintln!(
                "envc: startup profile '{startup}' has no directory under {}",
                Tilde(&dir)
            );
        }
    }

    println!();
    print_startup_line(&c, startup.as_deref())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Startup loading: the platform's half
// ---------------------------------------------------------------------------

/// What "startup loading" means on this platform.
///
/// POSIX shells read an rc file, so `apply` installs a hook into it. Windows has
/// no rc file that both cmd and PowerShell read, so `apply` puts the profile's
/// variables into the user environment instead and `revert` hands back what they
/// held before. Choosing the profile, remembering it and reporting on it are the
/// same either way -- which is why the commands no longer say which platform
/// they are running on.
trait Layer {
    /// Make the mechanism ready and describe it, for `init`. POSIX installs the
    /// hook here; Windows has nothing to install.
    fn prepare(&self) -> Result<Vec<String>>;

    /// Where startup loading lives, for `status` and `list`.
    fn describe(&self) -> Result<String>;

    /// Whether it is on right now.
    fn is_on(&self) -> Result<bool>;

    /// Turn it on for `profile`, describing what that did.
    fn apply(&self, profile: &str) -> Result<String>;

    /// Turn it off. `None` when it was already off, else what was undone.
    fn revert(&self) -> Result<Option<String>>;

    /// The line for "it was already off"; POSIX can name the file, Windows
    /// does not have one.
    fn already_off(&self) -> String;

    /// What switching it on means for the shell in front of the user.
    fn applied_hint(&self, profile: &str) -> Vec<String>;

    /// Trailing advice for `init`.
    fn advice(&self, already_on: bool, quiet: bool) -> Result<Vec<String>>;

    /// Complain about anything that would stop this working.
    fn check_environment(&self);
}

/// This platform's layer. The only place the platform is named.
fn layer() -> &'static dyn Layer {
    #[cfg(windows)]
    {
        &Windows
    }
    #[cfg(not(windows))]
    {
        &Posix
    }
}

/// POSIX: a hook in the login shell's rc file.
#[cfg(not(windows))]
struct Posix;

#[cfg(not(windows))]
impl Layer for Posix {
    fn prepare(&self) -> Result<Vec<String>> {
        let rc = rc_file()?;
        let already = rc_is_enabled()?;
        if !already {
            rc_enable()?;
        }
        Ok(vec![
            format!("shell:   {} ({})", detect_shell().name(), detection_source()),
            format!("rc file: {}", Tilde(rc)),
            format!(
                "hook:    {}",
                if already { "already installed" } else { "installed" }
            ),
        ])
    }

    fn describe(&self) -> Result<String> {
        Ok(Tilde(rc_file()?).to_string())
    }

    fn is_on(&self) -> Result<bool> {
        rc_is_enabled()
    }

    fn apply(&self, _profile: &str) -> Result<String> {
        // The hook loads whatever ~/.envc/startup points at, so none of this
        // depends on which profile it is.
        let rc = rc_file()?;
        Ok(match rc_enable()? {
            EnableOutcome::Installed => format!("installed the hook into {}", Tilde(rc)),
            EnableOutcome::Refreshed => format!("refreshed the hook in {}", Tilde(rc)),
        })
    }

    fn revert(&self) -> Result<Option<String>> {
        if !rc_disable()? {
            return Ok(None);
        }
        Ok(Some(format!("removed the hook from {}", Tilde(rc_file()?))))
    }

    fn already_off(&self) -> String {
        match rc_file() {
            Ok(rc) => format!("startup loading was already disabled ({} has no hook)", Tilde(rc)),
            Err(_) => "startup loading was already disabled".to_string(),
        }
    }

    fn applied_hint(&self, profile: &str) -> Vec<String> {
        vec![format!(
            "new shells load it; `envc activate {profile}` for this one"
        )]
    }

    fn advice(&self, already_on: bool, _quiet: bool) -> Result<Vec<String>> {
        if already_on {
            return Ok(Vec::new());
        }
        Ok(vec![format!("open a new shell, or `source {}`.", Tilde(rc_file()?))])
    }

    fn check_environment(&self) {
        warn_if_not_on_path();
    }
}

/// Windows: the profile's variables, written permanently.
#[cfg(windows)]
struct Windows;

#[cfg(windows)]
impl Layer for Windows {
    fn prepare(&self) -> Result<Vec<String>> {
        Ok(vec![
            format!("shell:   {}", Shell::detect().name()),
            "startup: user environment (no rc file to patch)".to_string(),
        ])
    }

    fn describe(&self) -> Result<String> {
        Ok("user environment".to_string())
    }

    fn is_on(&self) -> Result<bool> {
        Ok(UserStack::load()?.is_some())
    }

    /// There is no rc file that cmd and PowerShell both read, so the variables
    /// go into the user environment, where every process started afterwards
    /// sees them. What they held before is recorded first, because `revert` has
    /// to be able to put that back.
    fn apply(&self, profile: &str) -> Result<String> {
        let assignments = parse_env_file(&profile_env_file(profile)?)?;
        if assignments.is_empty() {
            return Err(anyhow!("profile '{profile}' sets no variables"));
        }

        let keys: Vec<&str> = assignments.iter().map(|a| a.key.as_str()).collect();
        let before = read_user_vars(&keys)?;

        let entries = assignments
            .iter()
            .map(|a| Entry {
                key: a.key.clone(),
                prev: before.get(&a.key).cloned().flatten(),
                now: Some(a.value.clone()),
            })
            .collect();

        let pairs: Vec<(String, Option<String>)> = assignments
            .iter()
            .map(|a| (a.key.clone(), Some(a.value.clone())))
            .collect();

        write_user_vars(&pairs)?;
        UserStack(Stack::new(profile, entries)).save()?;

        Ok(format!(
            "wrote {} variables to your user environment",
            assignments.len()
        ))
    }

    fn revert(&self) -> Result<Option<String>> {
        let Some(user_stack) = UserStack::load()? else {
            return Ok(None);
        };

        // `None` on an entry means the variable did not exist at the user
        // level, so this removes it rather than setting it empty.
        let pairs: Vec<(String, Option<String>)> = user_stack
            .0
            .entries
            .iter()
            .map(|e| (e.key.clone(), e.prev.clone()))
            .collect();

        write_user_vars(&pairs)?;
        UserStack::remove()?;
        Ok(Some(format!("restored {} user variables", pairs.len())))
    }

    fn already_off(&self) -> String {
        "startup loading was already disabled".to_string()
    }

    fn applied_hint(&self, _profile: &str) -> Vec<String> {
        vec![
            "new shells and programs see them; this one does not".to_string(),
            "`envc disable` puts the previous values back".to_string(),
        ]
    }

    fn advice(&self, _already_on: bool, quiet: bool) -> Result<Vec<String>> {
        if quiet {
            return Ok(Vec::new());
        }
        Ok(vec![
            "to apply a profile to just this shell:".to_string(),
            "    cmd        envc activate work && call \"%TEMP%\\envc\\activate.cmd\""
                .to_string(),
            "    powershell envc activate work | Invoke-Expression".to_string(),
        ])
    }

    fn check_environment(&self) {
        // Nothing to check: `enable` does not depend on the shell finding the
        // binary on PATH.
    }
}

fn print_startup_line(c: &Palette, startup: Option<&str>) -> Result<()> {
    let enabled = layer().is_on()?;
    let state = if enabled {
        c.green("enabled")
    } else {
        c.yellow("disabled")
    };
    let suffix = if enabled {
        format!(" ({})", layer().describe()?)
    } else {
        String::new()
    };
    println!("startup loading: {state}{suffix}");

    match (enabled, startup) {
        (true, Some(name)) => println!("startup profile: {}", c.bold(name)),
        (true, None) => println!("startup profile: (none -- `envc enable <name>`)"),
        // Remembered but not loading: saying "work" alone would read as if new
        // shells were about to load it.
        (false, Some(name)) => println!(
            "startup profile: {} {}",
            c.bold(name),
            c.dim("(remembered, loading off)")
        ),
        (false, None) => println!("startup profile: (none)"),
    }

    // The other half, when it disagrees with the startup choice.
    if let Some(current) = Env::get().active.as_deref() {
        if enabled && startup == Some(current) {
            return Ok(());
        }
        println!("this shell:      {current}");
    }
    Ok(())
}

fn cmd_delete(name: &str, force: bool, quiet: bool) -> Result<()> {
    validate_profile_name(name)?;

    let dir = profile_dir(name)?;
    if !dir.is_dir() {
        return Err(anyhow!("no such profile: {name}"));
    }

    let active = Stack::load()?.map(|s| s.profile);
    let is_active = active.as_deref() == Some(name);
    let is_startup = Startup::load()?.map(|s| s.profile).as_deref() == Some(name);

    if (is_active || is_startup) && !force {
        let why = if is_active {
            "currently active; run `envc deactivate` first"
        } else {
            "the startup profile; run `envc disable` first"
        };
        return Err(anyhow!(
            "profile '{name}' is {why}, or pass --force"
        ));
    }

    std::fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", Tilde(&dir)))?;

    if is_startup {
        Startup::remove()?;
        notify(
            quiet,
            format!("cleared the startup profile ('{name}' deleted)"),
        );
        if !quiet {
            eprintln!("envc: warning: new shells load nothing until you pick one:");
            eprintln!("envc:          envc enable <name>");
        }
    }

    if is_active {
        Stack::remove()?;
        notify(quiet, format!("cleared the restore stack ('{name}')"));
        if !quiet {
            eprintln!("envc: warning: this shell still holds those variables;");
            eprintln!("envc:          `envc deactivate` drops them.");
        }
    }

    println!("deleted profile '{name}' ({})", Tilde(&dir));
    Ok(())
}

fn cmd_activate(name: &str, quiet: bool, shell: Shell) -> Result<()> {
    validate_profile_name(name)?;

    let file = profile_env_file(name)?;
    if !file.is_file() {
        return Err(anyhow!(
            "no such profile: '{name}' (expected {})",
            Tilde(&file)
        ));
    }
    let assignments = parse_env_file(&file)?;

    let current = Env::get().vars.clone();
    let previous = Stack::load()?;

    let mut lines = Vec::new();
    // Peel the previous profile off first, so the values we remember are the
    // ones the shell had before *any* envc profile was applied.
    let base = match &previous {
        Some(old) => {
            lines.extend(old.restore_lines(shell));
            old.base_env(&current)
        }
        None => current.clone(),
    };

    let entries = Stack::entries_from(&base, &assignments);
    for a in &assignments {
        lines.push(shell.assign(&a.key, &a.value));
    }
    lines.push(shell.assign(ENVC_ACTIVE_VAR, name));
    shell.deliver("activate", &lines)?;

    Stack::new(name, entries).save()?;

    let what = match &previous {
        None => format!("activated '{name}'"),
        Some(old) if old.profile == name => format!("reloaded '{name}'"),
        Some(old) => format!("switched '{}' -> '{name}'", old.profile),
    };
    notify(quiet, format!("{what} ({} vars)", assignments.len()));
    print_activation_hint(quiet, &format!("envc activate {name}"), shell);
    Ok(())
}

fn cmd_deactivate(quiet: bool, shell: Shell) -> Result<()> {
    match Stack::load()? {
        Some(stack) => {
            let mut lines = stack.restore_lines(shell);
            lines.push(shell.unset(ENVC_ACTIVE_VAR));
            shell.deliver("deactivate", &lines)?;
            Stack::remove()?;

            notify(
                quiet,
                format!(
                    "deactivated '{}' ({} vars restored)",
                    stack.profile,
                    stack.entries.len()
                ),
            );
            print_activation_hint(quiet, "envc deactivate", shell);
        }
        None => {
            if Env::get().active.is_some() {
                shell.deliver("deactivate", &[shell.unset(ENVC_ACTIVE_VAR)])?;
                notify(quiet, "nothing on the restore stack; cleared ENVC_ACTIVE only");
            } else {
                notify(quiet, "no profile is active in this shell");
            }
        }
    }
    Ok(())
}

/// Re-apply the startup profile in a fresh shell. Called by the ~/.bashrc hook.
///
/// Unlike `activate` there is no previous stack to peel off: a new shell starts
/// from the login environment, which is exactly the base we want to remember.
/// The stack written here is only there so `deactivate` can undo the autoload;
/// what gets loaded is decided by the startup file, never by the stack.
fn cmd_autoload(shell: Shell) -> Result<()> {
    let Some(profile) = remembered_startup()? else {
        return Ok(());
    };

    let file = profile_env_file(&profile)?;
    if !file.is_file() {
        eprintln!(
            "envc: startup profile '{profile}' is selected but {} is missing; skipping autoload",
            Tilde(&file)
        );
        return Ok(());
    }

    let assignments = parse_env_file(&file)?;
    let current = Env::get().vars.clone();

    let mut lines = Vec::new();
    for a in &assignments {
        lines.push(shell.assign(&a.key, &a.value));
    }
    lines.push(shell.assign(ENVC_ACTIVE_VAR, &profile));
    shell.deliver("autoload", &lines)?;

    Stack::new(&profile, Stack::entries_from(&current, &assignments)).save()?;
    Ok(())
}

/// First-time setup: get this platform's startup mechanism in place and report
/// what the user has and what to do next.
fn cmd_init(quiet: bool) -> Result<()> {
    let layer = layer();
    let already_on = layer.is_on()?;

    for line in layer.prepare()? {
        println!("{line}");
    }

    match Startup::load()? {
        Some(s) => println!("profile: {}", s.profile),
        None => println!("profile: none -- `envc enable <name>` picks one"),
    }

    layer.check_environment();

    let advice = layer.advice(already_on, quiet)?;
    if !advice.is_empty() {
        println!();
        for line in advice {
            println!("{line}");
        }
    }
    if !quiet {
        println!();
        println!("next: envc create work, then envc enable work");
    }
    Ok(())
}

/// Choose the profile new shells load, and make sure this platform's mechanism
/// for loading it is in place.
///
/// This is the startup half of envc: it changes what the *next* shell does and
/// leaves the current one alone. `activate` is the other half.
fn cmd_enable(name: Option<&str>) -> Result<()> {
    // The name is moved into the state and read back from it below, so nothing
    // is cloned on the way to those messages.
    let startup = Startup {
        profile: resolve_startup_profile(name)?,
    };
    let layer = layer();

    // Applied before it is remembered: a mechanism that failed to install
    // should not leave a choice recorded that nothing will act on.
    let what = layer.apply(&startup.profile)?;
    startup.save()?;

    println!("startup loading enabled: {}", startup.profile);
    println!("  {what}");
    for line in layer.applied_hint(&startup.profile) {
        println!("  {line}");
    }
    layer.check_environment();
    Ok(())
}

/// Which profile `enable` should select: the one named on the command line,
/// else the one already remembered. Either way it has to exist.
fn resolve_startup_profile(name: Option<&str>) -> Result<String> {
    let profile = match name {
        Some(name) => {
            validate_profile_name(name)?;
            name.to_string()
        }
        None => remembered_startup()?.ok_or_else(|| {
            anyhow!("no startup profile chosen yet; run `envc enable <name>`".to_string())
        })?,
    };

    let file = profile_env_file(&profile)?;
    if !file.is_file() {
        return Err(anyhow!(
            "no such profile: '{profile}' (expected {})",
            Tilde(&file)
        ));
    }
    Ok(profile)
}

/// The hook is useless if new shells cannot find the binary, and that is an easy
/// mistake to make after a plain `cargo build`.
#[cfg(not(windows))]
fn warn_if_not_on_path() {
    if binary_on_path() {
        return;
    }
    eprintln!();
    eprintln!("envc: warning: not on PATH, so the hook cannot fire in new shells.");
    eprintln!(
        "envc:          install -Dm755 {} {}",
        current_exe(),
        suggested_install_path()
    );
}

/// `~/.local/bin` on Linux, `/usr/local/bin` on macOS (where the former is
/// rarely on PATH).
#[cfg(not(windows))]
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

/// Stop new shells from loading a profile. The current shell keeps what it has
/// applied, and the choice is remembered for a later `enable`.
fn cmd_disable(quiet: bool) -> Result<()> {
    let layer = layer();
    let Some(what) = layer.revert()? else {
        if !quiet {
            println!("{}", layer.already_off());
            println!("  run `envc enable` to turn it back on");
        }
        return Ok(());
    };

    println!("startup loading disabled");
    println!("  {what}");
    if let Some(startup) = Startup::load()? {
        println!(
            "  '{}' remembered; a bare `envc enable` turns it back on",
            startup.profile
        );
    }
    if !quiet {
        println!("  this shell keeps its profile -- `envc deactivate` drops it.");
    }
    Ok(())
}

fn cmd_stack() -> Result<()> {
    let path = stack_path()?;
    let c = Palette::new();
    match Stack::load()? {
        Some(stack) => {
            println!(
                "{} '{}' active, {} in the stack",
                c.bold("envc:"),
                c.bold(&stack.profile),
                stack.entries.len()
            );
            println!("{}", Tilde(&path));
            println!();
            print!("{}", stack.render());
        }
        None => {
            println!("no profile active; {} is absent", Tilde(&path));
        }
    }
    Ok(())
}

fn cmd_status() -> Result<()> {
    let shell = Shell::detect();
    let c = Palette::new();
    let home = envc_home()?;
    let stack = Stack::load()?;
    let shell_active = Env::get().active.clone();

    println!("{} {}", c.bold("envc"), env!("CARGO_PKG_VERSION"));
    println!("  {:<17}{}", "home", Tilde(home));
    println!("  {:<17}{}", "shell", shell.name());

    let enabled = layer().is_on()?;
    let state = if enabled {
        c.green("enabled")
    } else {
        c.yellow("disabled")
    };
    println!("  {:<17}{} ({})", "startup loading", state, layer().describe()?);

    // `Startup::load` and not `remembered_startup`: reporting must not migrate.
    let startup = Startup::load()?.map(|s| s.profile);
    match (&enabled, &startup) {
        (true, Some(s)) => println!("  {:<17}{}", "startup profile", c.bold(s)),
        (true, None) => println!(
            "  {:<17}{}",
            "startup profile",
            c.dim("(none -- `envc enable <name>`)")
        ),
        (false, Some(s)) => println!(
            "  {:<17}{} {}",
            "startup profile",
            c.bold(s),
            c.dim("(remembered, loading off)")
        ),
        (false, None) => println!("  {:<17}{}", "startup profile", c.dim("(none)")),
    }

    match &shell_active {
        Some(name) => println!("  {:<17}{}", "this shell", c.bold(name)),
        None => println!("  {:<17}{}", "this shell", c.dim("(none)")),
    }

    match &stack {
        Some(s) => println!(
            "  {:<17}{} vars ({})",
            "restore stack",
            s.entries.len(),
            Tilde(&stack_path()?)
        ),
        None => println!("  {:<17}{}", "restore stack", c.dim("(empty)")),
    }

    // The two halves only disagree when this shell is not showing what the next
    // shell will load, which is exactly when the user needs to be told.
    if enabled {
        if let (Some(startup), Some(cur)) = (&startup, &shell_active) {
            if startup != cur {
                eprintln!();
                eprintln!("envc: note: this shell has '{cur}', new shells load '{startup}'.");
                eprintln!(
                    "envc:       `envc activate {startup}` to match, \
                     `envc enable {cur}` to switch."
                );
            }
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
    println!("  {:<17}{} in {}", "profiles", count, Tilde(&profiles));
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
    io::stdout().is_terminal() && !Env::get().wrapped
}

/// Printed when shell code was generated but nothing is going to eval it.
fn print_activation_hint(quiet: bool, command: &str, shell: Shell) {
    // Only shells that evaluate stdout need telling how; cmd got a batch file
    // and `Shell::write_batch` has already said which one.
    if quiet || !shell.evaluates_stdout() || !hint_wanted() {
        return;
    }
    eprintln!();
    eprintln!("envc: this shell is unchanged until you evaluate that:");
    eprintln!("envc:     eval \"$({command})\"");
    eprintln!("envc: `envc enable` installs a wrapper so that is not needed.");
}

#[cfg(not(windows))]
fn binary_on_path() -> bool {
    let Some(path) = &Env::get().path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| dir.join("envc").is_file())
}

#[cfg(not(windows))]
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
            on: io::stdout().is_terminal() && !Env::get().no_color,
        }
    }

    /// `impl Display` rather than `&str` so callers can pass a `Tilde` (or any
    /// other lazy wrapper) without materialising a string first.
    fn paint(&self, code: &str, s: impl fmt::Display) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

/// One method per color, so adding a color is a single line.
macro_rules! palette_colors {
    ($($name:ident => $code:literal),+ $(,)?) => {
        impl Palette {
            $(fn $name(&self, s: impl fmt::Display) -> String {
                self.paint($code, s)
            })+
        }
    };
}

palette_colors! {
    bold => "1",
    dim => "2",
    green => "32",
    yellow => "33",
    red => "31",
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

    /// Which shell syntax to emit: bash, powershell or cmd
    #[arg(long, global = true, value_name = "SHELL")]
    shell: Option<String>,

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

    /// Choose the profile new shells load and install the startup hook
    Enable {
        /// Profile new shells load; defaults to the one chosen previously
        name: Option<String>,
    },

    /// Stop new shells from loading a profile (this shell is left alone)
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
    let shell = Shell::resolve(cli.shell.as_deref())?;
    match cli.command {
        Command::Create { name, force } => cmd_create(&name, force),
        Command::List => cmd_list(),
        Command::Delete { name, force } => cmd_delete(&name, force, quiet),
        Command::Activate { name } => cmd_activate(&name, quiet, shell),
        Command::Deactivate => cmd_deactivate(quiet, shell),
        Command::Init => cmd_init(quiet),
        Command::Enable { name } => cmd_enable(name.as_deref()),
        Command::Disable => cmd_disable(quiet),
        Command::Stack => cmd_stack(),
        Command::Status => cmd_status(),
        Command::Autoload => cmd_autoload(shell),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `{:#}` prints the whole context chain, so a wrapped io::Error
            // still reads as "cannot read ~/.envc/stack: No such file...".
            eprintln!("envc: {e:#}");
            ExitCode::FAILURE
        }
    }
}

