use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};

pub type Result<T> = anyhow::Result<T>;




#[cfg(windows)]
const HOME_KEYS: &[&str] = &["HOME", "USERPROFILE"];
#[cfg(not(windows))]
const HOME_KEYS: &[&str] = &["HOME"];

const PROFILE_ENV_FILE: &str = ".env";

pub struct Env {
    pub vars: HashMap<String, String>,
    pub home: Option<PathBuf>,
    pub login_shell: Option<String>,
    pub ps_module_path: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub system_root: Option<PathBuf>,
    pub temp_dir: PathBuf,
    pub session: String,
    pub active: Option<String>,
    #[cfg_attr(windows, allow(dead_code))]
    pub path: Option<OsString>,
}

impl Env {
    pub fn read() -> Env {
        let path = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Env {
            vars: std::env::vars().collect(),
            home: HOME_KEYS.iter().find_map(|k| path(k)).map(PathBuf::from),
            login_shell: var("SHELL"),
            ps_module_path: std::env::var_os("PSModulePath").is_some(),
            system_root: path("SystemRoot").map(PathBuf::from),
            temp_dir: std::env::temp_dir(),
            session: Env::session_id(),
            active: var("ENVC_ACTIVE"),
            path: std::env::var_os("PATH"),
        }
    }

    #[cfg(unix)]
    fn session_id() -> String {
        unsafe { libc::getsid(0) }.to_string()
    }

    #[cfg(windows)]
    fn session_id() -> String {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };

        let me = std::process::id();
        let mut parent = None;
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return "console".to_string();
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>().try_into().unwrap_or(0);
            let mut ok = Process32FirstW(snapshot, &mut entry);
            while ok != 0 {
                if entry.th32ProcessID == me {
                    parent = Some(entry.th32ParentProcessID);
                    break;
                }
                ok = Process32NextW(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
        }
        parent.map_or_else(|| "console".to_string(), |p| p.to_string())
    }

    fn not_set() -> anyhow::Error {
        anyhow!("{} is not set", HOME_KEYS.join(" / "))
    }

    pub fn home(&self) -> Result<&Path> {
        self.home.as_deref().ok_or_else(Env::not_set)
    }

    pub fn envc_home(&self) -> Result<PathBuf> {
        Ok(self.home()?.join(".envc"))
    }

    pub fn profiles_dir(&self) -> Result<PathBuf> {
        Ok(self.envc_home()?.join("profiles"))
    }

    pub fn profile_dir(&self, name: &str) -> Result<PathBuf> {
        Ok(self.profiles_dir()?.join(name))
    }

    pub fn profile_env_file(&self, name: &str) -> Result<PathBuf> {
        Ok(self.profile_dir(name)?.join(PROFILE_ENV_FILE))
    }

    pub fn existing_profile(&self, name: &str) -> Result<PathBuf> {
        let file = self.profile_env_file(name)?;
        if !file.is_file() {
            return Err(anyhow!(
                "no such profile: '{name}' (expected {})",
                self.tilde(&file)
            ));
        }
        Ok(file)
    }

    pub fn state_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.envc_home()?.join(name))
    }

    pub fn tilde<'a>(&'a self, path: &'a Path) -> Tilde<'a> {
        Tilde {
            path,
            home: self.home.as_deref(),
        }
    }

    pub fn profile_names(&self) -> Result<Vec<String>> {
        let dir = self.profiles_dir()?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(anyhow!("cannot read {}: {e}", self.tilde(&dir))),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| anyhow!("cannot read {}: {e}", self.tilde(&dir)))?;
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }
}

pub struct Tilde<'a> {
    path: &'a Path,
    home: Option<&'a Path>,
}

impl fmt::Display for Tilde<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.home.and_then(|home| self.path.strip_prefix(home).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => f.write_str("~"),
            Some(rest) => write!(f, "~/{}", rest.display()),
            None => write!(f, "{}", self.path.display()),
        }
    }
}

pub struct ProfileName<'a>(pub &'a str);

impl<'a> TryFrom<&'a str> for ProfileName<'a> {
    type Error = anyhow::Error;

    fn try_from(name: &'a str) -> Result<Self> {
        let why = if name.is_empty() {
            "must not be empty"
        } else if name.len() > 64 {
            "must be at most 64 characters"
        } else if name.starts_with('.') {
            "must not start with a dot"
        } else if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            "only letters, digits, '_', '-' and '.' are allowed"
        } else {
            return Ok(ProfileName(name));
        };
        Err(anyhow!("invalid profile name {name:?}: {why}"))
    }
}



#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Default)]
pub struct Config {
    pub shell: Option<String>,
    pub rc: Option<PathBuf>,
    pub color: Color,
}

impl Config {
    pub fn load(env: &Env) -> Result<Config> {
        let Some(home) = env.home.as_deref() else {
            return Ok(Config::default());
        };
        let path = home.join(".envc").join("config");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(anyhow!("cannot read {}: {e}", env.tilde(&path))),
        };

        let mut config = Config::default();
        for (idx, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fail = |msg: String| anyhow!("{}:{}: {msg}", env.tilde(&path), idx + 1);
            let Some((key, value)) = line.split_once('=') else {
                return Err(fail("expected `key = value`".into()));
            };
            let value = value.trim();
            match key.trim() {
                "shell" => config.shell = Some(value.to_string()),
                "rc" => {
                    config.rc = Some(match value.strip_prefix("~/") {
                        Some(rest) => home.join(rest),
                        None => PathBuf::from(value),
                    })
                }
                "color" => {
                    config.color = match value {
                        "auto" => Color::Auto,
                        "always" => Color::Always,
                        "never" => Color::Never,
                        other => {
                            return Err(fail(format!(
                                "color must be auto, always or never, not {other:?}"
                            )))
                        }
                    }
                }
                other => return Err(fail(format!("unknown setting {other:?}"))),
            }
        }
        Ok(config)
    }
}



pub const ENVC_ACTIVE_VAR: &str = "ENVC_ACTIVE";

pub trait Identifier {
    fn is_identifier(&self) -> bool;
}

impl Identifier for str {
    fn is_identifier(&self) -> bool {
        let mut chars = self.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Posix,
    PowerShell,
    Cmd,
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Shell::Posix => "bash",
            Shell::PowerShell => "powershell",
            Shell::Cmd => "cmd",
        })
    }
}

impl Shell {
    pub fn resolve(flag: Option<&str>, config: &Config, env: &Env) -> Result<Self> {
        match flag.or(config.shell.as_deref()).map(str::trim) {
            None => Ok(Shell::detect(env)),
            Some("bash" | "zsh" | "sh" | "posix") => Ok(Shell::Posix),
            Some("powershell" | "pwsh" | "ps") => Ok(Shell::PowerShell),
            Some("cmd" | "cmd.exe" | "bat" | "batch") => Ok(Shell::Cmd),
            Some(other) => Err(anyhow!(
                "unknown shell {other:?}; expected bash, powershell or cmd"
            )),
        }
    }

    pub fn detect(env: &Env) -> Self {
        match (cfg!(windows), env.ps_module_path) {
            (false, _) => Shell::Posix,
            (true, true) => Shell::PowerShell,
            (true, false) => Shell::Cmd,
        }
    }

    fn is_safe_unquoted(c: char) -> bool {
        c.is_ascii_alphanumeric()
            || matches!(c, '%' | '+' | ',' | '-' | '.' | '/' | ':' | '=' | '@' | '_' | '^')
    }

    fn quote_posix(value: &str) -> String {
        if value.is_empty() {
            return "''".to_string();
        }
        if value.chars().all(Shell::is_safe_unquoted) {
            return value.to_string();
        }
        format!("'{}'", value.replace('\'', r"'\''"))
    }

    pub fn quote_powershell(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn set_cmd(key: &str, value: &str) -> String {
        format!("set \"{key}={}\"", value.replace('%', "%%"))
    }

    pub fn assign(self, key: &str, value: &str) -> String {
        match self {
            Shell::Posix => format!("export {key}={}", Shell::quote_posix(value)),
            Shell::PowerShell => format!("$env:{key}={}", Shell::quote_powershell(value)),
            Shell::Cmd => Shell::set_cmd(key, value),
        }
    }

    pub fn unset(self, key: &str) -> String {
        match self {
            Shell::Posix => format!("unset {key}"),
            Shell::PowerShell => format!("Remove-Item Env:{key} -ErrorAction SilentlyContinue"),
            Shell::Cmd => Shell::set_cmd(key, ""),
        }
    }

    pub fn set_or_unset(self, key: &str, value: Option<&str>) -> String {
        match value {
            Some(v) => self.assign(key, v),
            None => self.unset(key),
        }
    }

    pub fn evaluates_stdout(self) -> bool {
        self != Shell::Cmd
    }

    pub fn deliver(self, env: &Env, what: &str, lines: &[String]) -> Result<()> {
        if self == Shell::Cmd {
            return self.write_batch(env, what, lines);
        }
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
        Ok(())
    }

    fn write_batch(self, env: &Env, what: &str, lines: &[String]) -> Result<()> {
        let dir = env.temp_dir.join("envc");
        let path = dir.join(format!("{what}.cmd"));
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", env.tilde(&dir)))?;
        let body: String = std::iter::once("@echo off")
            .chain(lines.iter().map(String::as_str))
            .flat_map(|line| [line, "\r\n"])
            .collect();
        std::fs::write(&path, body).with_context(|| format!("cannot write {}", env.tilde(&path)))?;

        eprintln!("envc: wrote {}", env.tilde(&path));
        eprintln!("envc: in cmd, run: call \"{}\"", path.display());
        Ok(())
    }
}



type Vars = HashMap<String, String>;
type Step<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub key: String,
    pub value: String,
}

enum ValueError {
    Unterminated(char),
    Other(String),
}

impl From<String> for ValueError {
    fn from(msg: String) -> Self {
        ValueError::Other(msg)
    }
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::Unterminated(q) => write!(f, "unterminated {q} quote"),
            ValueError::Other(msg) => f.write_str(msg),
        }
    }
}

impl Assignment {
    pub fn load(env: &Env, path: &Path, base: &Vars) -> Result<Vec<Assignment>> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", env.tilde(path)))?;
        Assignment::parse(&content, env.tilde(path), base)
    }

    pub fn parse(content: &str, origin: impl fmt::Display, base: &Vars) -> Result<Vec<Assignment>> {
        let mut expander = Expander { env: base.clone() };
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

            let line = match line.strip_prefix("export") {
                Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
                _ => line,
            };
            let Some((raw_key, raw_value)) = line.split_once('=') else {
                return Err(fail("expected `KEY=value`"));
            };

            let key = raw_key.trim();
            if !key.is_identifier() {
                return Err(fail(&format!("{key:?} is not a valid variable name")));
            }

            let mut raw = raw_value.trim_start().to_string();
            let value = loop {
                match expander.value(&raw) {
                    Ok(value) => break Ok(value),
                    Err(ValueError::Unterminated(q)) if idx >= lines.len() => {
                        break Err(fail(&format!("in the value of {key}: unterminated {q} quote")))
                    }
                    Err(ValueError::Unterminated(_)) => {
                        raw.push('\n');
                        raw.push_str(lines[idx]);
                        idx += 1;
                    }
                    Err(e) => break Err(fail(&format!("in the value of {key}: {e}"))),
                }
            }?;

            expander.env.insert(key.to_string(), value.clone());
            out.push(Assignment {
                key: key.to_string(),
                value,
            });
        }

        Ok(out)
    }
}

struct Expander {
    env: Vars,
}

impl Expander {
    fn value(&self, raw: &str) -> std::result::Result<String, ValueError> {
        let chars: Vec<char> = raw.chars().collect();
        let mut out = String::new();
        let mut pending = String::new();
        let mut i = 0;
        let mut quote: Option<char> = None;
        let mut at_boundary = true;
        let flush = |out: &mut String, pending: &mut String| {
            out.push_str(pending);
            pending.clear();
        };

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
                    out.push(c);
                    i += 1;
                    continue;
                }
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
                    let expanded = match (quote, next) {
                        (Some('"'), 'n') => '\n',
                        (Some('"'), 't') => '\t',
                        (Some('"'), 'r') => '\r',
                        (_, other) => other,
                    };
                    flush(&mut out, &mut pending);
                    out.push(expanded);
                    at_boundary = false;
                    i += 1;
                }
                '$' => {
                    let (value, next_i) = self.expand(&chars, i)?;
                    flush(&mut out, &mut pending);
                    at_boundary = at_boundary && value.is_empty();
                    out.push_str(&value);
                    i = next_i;
                }
                '#' if quote != Some('"') && (i == 0 || chars[i - 1].is_whitespace()) => break,
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

    fn lookup(&self, name: &str) -> String {
        self.env.get(name).cloned().unwrap_or_default()
    }

    fn expand(&self, chars: &[char], start: usize) -> Step<(String, usize)> {
        let mut i = start + 1;
        let Some(&first) = chars.get(i) else {
            return Ok(("$".to_string(), i));
        };

        if first != '{' {
            if !(first.is_ascii_alphabetic() || first == '_') {
                return Ok(("$".to_string(), i));
            }
            let name: String = chars[i..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                .collect();
            i += name.chars().count();
            return Ok((self.lookup(&name), i));
        }

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
            let skip = match (c, chars.get(i + 1)) {
                (':', Some('-')) => 2,
                ('-', _) => 1,
                _ => 0,
            };
            if skip > 0 {
                let (text, next_i) = Expander::read_default(chars, i + skip)?;
                default = Some(text);
                i = next_i;
                break;
            }
            name.push(c);
            i += 1;
        }

        if !name.is_identifier() {
            return Err(format!("invalid variable name in `${{{name}}}`"));
        }

        let current = self.lookup(&name);
        match default {
            Some(d) if current.is_empty() => Ok((self.expand_default(&d)?, i)),
            _ => Ok((current, i)),
        }
    }

    fn expand_default(&self, raw: &str) -> Step<String> {
        let chars: Vec<char> = raw.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '$' {
                let (value, next_i) = self.expand(&chars, i)?;
                out.push_str(&value);
                i = next_i;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        Ok(out)
    }

    fn read_default(chars: &[char], start: usize) -> Step<(String, usize)> {
        let mut text = String::new();
        let mut i = start;
        let mut depth = 0usize;
        loop {
            let Some(&c) = chars.get(i) else {
                return Err("unterminated `${`".to_string());
            };
            match c {
                '}' if depth == 0 => return Ok((text, i + 1)),
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            text.push(c);
            i += 1;
        }
    }
}



const STACK_FORMAT_VERSION: u32 = 2;

pub trait StateFile: Sized {
    const NAME: &str;

    fn render(&self) -> String;

    fn from_content(env: &Env, content: &str, path: &std::path::Path) -> Result<Option<Self>>;

    fn path(env: &Env) -> Result<PathBuf> {
        env.state_path(Self::NAME)
    }

    fn load(env: &Env) -> Result<Option<Self>> {
        let path = Self::path(env)?;
        match std::fs::read_to_string(&path) {
            Ok(content) => Self::from_content(env, &content, &path),
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
            Err(e) => Err(anyhow!("cannot read {}: {e}", env.tilde(&path))),
        }
    }

    fn save(&self, env: &Env) -> Result<()> {
        let path = Self::path(env)?;
        if let Some(dir) = path.parent() {
            if dir.is_file() {
                std::fs::remove_file(dir).with_context(|| format!("cannot remove {}", env.tilde(dir)))?;
            }
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", env.tilde(dir)))?;
        }
        std::fs::write(&path, self.render()).with_context(|| format!("cannot write {}", env.tilde(&path)))
    }

    fn remove(env: &Env) -> Result<()> {
        let path = Self::path(env)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow!("cannot remove {}: {e}", env.tilde(&path))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub prev: Option<String>,
    pub now: Option<String>,
}

impl Entry {
    fn side(key: &str, value: Option<&str>) -> String {
        let Some(value) = value else {
            return key.to_string();
        };
        let mut s = format!("{key}=");
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

    fn unescape(value: &str) -> String {
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
                Some(other) => {
                    s.push('\\');
                    s.push(other);
                }
                None => s.push('\\'),
            }
        }
        s
    }

    fn split_side(side: &str) -> std::result::Result<(String, Option<String>), String> {
        let (key, value) = match side.split_once('=') {
            Some((k, v)) => (k, Some(Entry::unescape(v))),
            None => (side, None),
        };
        if !key.is_identifier() {
            return Err(format!("{key:?} is not a valid variable name"));
        }
        Ok((key.to_string(), value))
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub profile: String,
    pub digest: Option<String>,
    pub pushed: Option<u64>,
    pub entries: Vec<Entry>,
}

impl Frame {
    pub fn capture(
        profile: &str,
        base: &HashMap<String, String>,
        assignments: &[Assignment],
        digest: Option<String>,
    ) -> Frame {
        Frame {
            profile: profile.to_string(),
            digest,
            pushed: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
            entries: assignments
                .iter()
                .map(|a| Entry {
                    key: a.key.clone(),
                    prev: base.get(&a.key).cloned(),
                    now: Some(a.value.clone()),
                })
                .collect(),
        }
    }

    pub fn assignments(&self) -> Vec<Assignment> {
        self.entries
            .iter()
            .filter_map(|e| {
                e.now.as_ref().map(|value| Assignment {
                    key: e.key.clone(),
                    value: value.clone(),
                })
            })
            .collect()
    }

    pub fn header(&self) -> String {
        format!(
            "{} {} {}",
            self.profile,
            self.digest.as_deref().unwrap_or("-"),
            self.pushed.map_or_else(|| "-".to_string(), |t| t.to_string())
        )
    }

    pub fn render_entries(&self, s: &mut String) {
        for e in &self.entries {
            s.push_str(&format!("-{}\n", Entry::side(&e.key, e.prev.as_deref())));
            s.push_str(&format!("+{}\n", Entry::side(&e.key, e.now.as_deref())));
        }
    }

    pub fn record(&self, env: &Env, action: &str) -> Result<()> {
        let path = env.state_path("history")?;
        let mut s = format!(
            "@@ {} {action} session={} {} @@\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            env.session,
            self.header()
        );
        self.render_entries(&mut s);
        std::fs::create_dir_all(env.envc_home()?)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open {}", env.tilde(&path)))?;
        file.write_all(s.as_bytes())
            .with_context(|| format!("cannot write {}", env.tilde(&path)))
    }

    pub fn apply_lines(&self, shell: Shell) -> impl Iterator<Item = String> + '_ {
        self.entries.iter().map(move |e| shell.set_or_unset(&e.key, e.now.as_deref()))
    }

    pub fn restore_lines(&self, shell: Shell) -> impl Iterator<Item = String> + '_ {
        self.entries.iter().rev().map(move |e| shell.set_or_unset(&e.key, e.prev.as_deref()))
    }

    fn put(env: &mut HashMap<String, String>, key: &str, value: Option<&String>) {
        match value {
            Some(v) => env.insert(key.to_string(), v.clone()),
            None => env.remove(key),
        };
    }

    pub fn apply(&self, env: &mut HashMap<String, String>) {
        self.entries.iter().for_each(|e| Frame::put(env, &e.key, e.now.as_ref()));
    }

    pub fn unwind(&self, env: &mut HashMap<String, String>) {
        self.entries.iter().rev().for_each(|e| Frame::put(env, &e.key, e.prev.as_ref()));
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stack {
    pub frames: Vec<Frame>,
}

impl fmt::Display for Stack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.frames.iter().map(|fr| fr.profile.as_str()).collect();
        f.write_str(&names.join(":"))
    }
}

impl Stack {
    pub fn position(&self, name: &str) -> Option<usize> {
        self.frames.iter().position(|f| f.profile == name)
    }

    pub fn var_count(&self) -> usize {
        self.frames.iter().map(|f| f.entries.len()).sum()
    }

    pub fn chain(&self) -> String {
        std::iter::once("base")
            .chain(self.frames.iter().map(|f| f.profile.as_str()))
            .collect::<Vec<_>>()
            .join(" > ")
    }

    pub fn active_line(&self, shell: Shell) -> String {
        match self.frames.is_empty() {
            true => shell.unset(ENVC_ACTIVE_VAR),
            false => shell.assign(ENVC_ACTIVE_VAR, &self.to_string()),
        }
    }

    pub fn store(&self, env: &Env) -> Result<()> {
        match self.frames.is_empty() {
            true => Stack::remove(env),
            false => self.save(env),
        }
    }

    pub fn parse(content: &str, origin: impl fmt::Display) -> Result<Stack> {
        let mut version = STACK_FORMAT_VERSION;
        let mut frames: Vec<Frame> = Vec::new();
        let mut pending: Option<(String, Option<String>)> = None;

        for (idx, raw) in content.lines().enumerate() {
            let lineno = idx + 1;
            let line = raw.trim_end();
            let fail = |msg: String| anyhow!("{origin}:{lineno}: {msg}");

            let mut open = |profile: &str, pending: &Option<_>| {
                if pending.is_some() {
                    return Err(fail("a '-' line was not followed by its '+' line".into()));
                }
                let mut words = profile.split_whitespace();
                frames.push(Frame {
                    profile: words.next().unwrap_or_default().to_string(),
                    digest: words.next().filter(|w| *w != "-").map(str::to_string),
                    pushed: words.next().and_then(|w| w.parse().ok()),
                    entries: Vec::new(),
                });
                Ok(())
            };

            if line.is_empty() || line.starts_with("---") || line.starts_with("+++") {
                continue;
            }
            if let Some(rest) = line.strip_prefix('#') {
                let rest = rest.trim();
                if let Some(v) = rest.strip_prefix("format ") {
                    version = v.trim().parse().map_err(|_| fail(format!("bad format number {v:?}")))?;
                } else if let Some(p) = rest.strip_prefix("active-profile ") {
                    open(p, &pending)?;
                }
                continue;
            }
            if let Some(v) = line.strip_prefix("format ") {
                version = v.trim().parse().map_err(|_| fail(format!("bad format number {v:?}")))?;
                continue;
            }
            if let Some(p) = line.strip_prefix("@@ ").and_then(|r| r.strip_suffix(" @@")) {
                open(p, &pending)?;
                continue;
            }
            if let Some(rest) = line.strip_prefix('-') {
                if pending.is_some() {
                    return Err(fail("a '-' line was not followed by its '+' line".into()));
                }
                pending = Some(Entry::split_side(rest).map_err(fail)?);
                continue;
            }
            if let Some(rest) = line.strip_prefix('+') {
                let (key, now) = Entry::split_side(rest).map_err(fail)?;
                let Some((prev_key, prev)) = pending.take() else {
                    return Err(fail("a '+' line has no matching '-' line".into()));
                };
                if prev_key != key {
                    return Err(fail(format!(
                        "'+' line for {key:?} does not match the preceding '-' line for {prev_key:?}"
                    )));
                }
                let Some(frame) = frames.last_mut() else {
                    return Err(fail("an entry comes before any `@@ <profile> @@` line".into()));
                };
                frame.entries.push(Entry { key, prev, now });
                continue;
            }
            return Err(fail(format!("unrecognised line: {line:?}")));
        }

        if let Some((key, _)) = pending {
            return Err(anyhow!("{origin}: '{key}' has a '-' line but no '+' line"));
        }
        if !(1..=STACK_FORMAT_VERSION).contains(&version) {
            return Err(anyhow!(
                "{origin}: written with stack format {version}, this build understands up to {STACK_FORMAT_VERSION}"
            ));
        }
        Ok(Stack { frames })
    }
}

impl StateFile for Stack {
    const NAME: &str = "stack";

    fn path(env: &Env) -> Result<PathBuf> {
        Ok(env.state_path(Self::NAME)?.join(&env.session))
    }

    fn render(&self) -> String {
        let mut s = format!("format {STACK_FORMAT_VERSION}\n");
        for frame in &self.frames {
            s.push_str(&format!("@@ {} @@\n", frame.header()));
            frame.render_entries(&mut s);
        }
        s
    }

    fn from_content(env: &Env, content: &str, path: &std::path::Path) -> Result<Option<Self>> {
        let stack = Stack::parse(content, env.tilde(path))?;
        let recorded = stack.to_string();
        match env.active.as_deref() {
            Some(active) if active == recorded => Ok((!stack.frames.is_empty()).then_some(stack)),
            Some(active) => {
                eprintln!(
                    "envc: warning: {} records '{recorded}' but this shell has '{active}'; \
                     another shell in this session changed it, so it is ignored",
                    env.tilde(path)
                );
                Ok(None)
            }
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Startup {
    pub profile: String,
}

impl StateFile for Startup {
    const NAME: &str = "startup";

    fn render(&self) -> String {
        format!("{}\n", self.profile)
    }

    fn from_content(_env: &Env, content: &str, _path: &std::path::Path) -> Result<Option<Self>> {
        Ok(content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|profile| Startup {
                profile: profile.to_string(),
            }))
    }
}

impl Startup {
    pub fn current(env: &Env) -> Result<Option<String>> {
        Ok(Startup::load(env)?.map(|s| s.profile))
    }

    pub fn remembered(env: &Env) -> Result<Option<String>> {
        if let Some(startup) = Startup::load(env)? {
            return Ok(Some(startup.profile));
        }
        let legacy = env.state_path(Stack::NAME)?;
        let profile = match std::fs::read_to_string(&legacy) {
            Ok(content) => Stack::parse(&content, env.tilde(&legacy))?.frames.into_iter().next(),
            Err(_) => None,
        };
        let Some(profile) = profile.map(|f| f.profile) else {
            return Ok(None);
        };
        if !env.profile_env_file(&profile)?.is_file() {
            return Ok(None);
        }
        Startup {
            profile: profile.clone(),
        }
        .save(env)?;
        eprintln!(
            "envc: '{profile}' is now the startup profile (it was selected before the upgrade; \
             `envc enable <name>` changes it)"
        );
        Ok(Some(profile))
    }
}

#[cfg(windows)]
pub struct UserStack(pub Frame);

#[cfg(windows)]
impl StateFile for UserStack {
    const NAME: &str = "user-stack";

    fn render(&self) -> String {
        Stack {
            frames: vec![self.0.clone()],
        }
        .render()
    }

    fn from_content(env: &Env, content: &str, path: &std::path::Path) -> Result<Option<Self>> {
        Ok(Stack::parse(content, env.tilde(path))?.frames.into_iter().next().map(UserStack))
    }
}

pub struct Source {
    pub assignments: Vec<Assignment>,
    pub digest: String,
    content: String,
}

impl Source {
    pub fn load(env: &Env, path: &Path, base: &HashMap<String, String>) -> Result<Source> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", env.tilde(path)))?;
        Ok(Source {
            assignments: Assignment::parse(&content, env.tilde(path), base)?,
            digest: Source::digest(&content),
            content,
        })
    }

    pub fn archive(&self, env: &Env, profile: &str) -> Result<()> {
        let dir = env.state_path("snapshots")?.join(profile);
        let path = dir.join(format!("{}.env", self.digest));
        if path.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", env.tilde(&dir)))?;
        std::fs::write(&path, &self.content).with_context(|| format!("cannot write {}", env.tilde(&path)))
    }

    fn digest(content: &str) -> String {
        let hash = content
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        format!("{hash:016x}")
    }
}

pub struct Drift {
    pub profile: String,
    pub missing: bool,
    pub dangling: Vec<String>,
    pub added: Vec<String>,
    pub changed: Vec<String>,
}

impl Drift {
    pub fn scan(env: &Env, frames: &[Frame]) -> Result<Vec<Drift>> {
        let mut drifts = Vec::new();
        for frame in frames {
            let Some(digest) = &frame.digest else {
                continue;
            };
            let path = env.profile_env_file(&frame.profile)?;
            let keys = frame.entries.iter().map(|e| e.key.clone());
            if !path.is_file() {
                drifts.push(Drift {
                    profile: frame.profile.clone(),
                    missing: true,
                    dangling: keys.collect(),
                    added: Vec::new(),
                    changed: Vec::new(),
                });
                continue;
            }
            let source = Source::load(env, &path, &env.vars)?;
            if &source.digest == digest {
                continue;
            }
            let now: HashMap<&str, &str> = source
                .assignments
                .iter()
                .map(|a| (a.key.as_str(), a.value.as_str()))
                .collect();
            let was: HashMap<&str, Option<&str>> =
                frame.entries.iter().map(|e| (e.key.as_str(), e.now.as_deref())).collect();
            drifts.push(Drift {
                profile: frame.profile.clone(),
                missing: false,
                dangling: keys.filter(|k| !now.contains_key(k.as_str())).collect(),
                added: source
                    .assignments
                    .iter()
                    .filter(|a| !was.contains_key(a.key.as_str()))
                    .map(|a| a.key.clone())
                    .collect(),
                changed: frame
                    .entries
                    .iter()
                    .filter(|e| now.get(e.key.as_str()).is_some_and(|v| Some(*v) != e.now.as_deref()))
                    .map(|e| e.key.clone())
                    .collect(),
            });
        }
        Ok(drifts)
    }
}

pub struct Ctx {
    pub env: Env,
    pub config: Config,
    pub quiet: bool,
    pub shell: Shell,
    pub wrapped: bool,
}

impl Ctx {
    pub fn notify(&self, msg: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("envc: {}", msg.as_ref());
        }
    }

    pub fn hint(&self, command: &str) {
        if self.quiet || !self.shell.evaluates_stdout() || !io::stdout().is_terminal() || self.wrapped {
            return;
        }
        eprintln!();
        eprintln!("envc: this shell is unchanged until you evaluate that:");
        eprintln!("envc:     eval \"$({command})\"");
        eprintln!("envc: `envc init` installs a wrapper so that is not needed.");
    }

    pub fn palette(&self) -> Palette {
        Palette {
            on: match self.config.color {
                Color::Always => true,
                Color::Never => false,
                Color::Auto => io::stdout().is_terminal(),
            },
        }
    }

    pub fn platform(&self) -> Platform<'_> {
        Platform::from(self)
    }

    pub fn confirm_drift(&self, frames: &[Frame], yes: bool) -> Result<()> {
        let drifts = Drift::scan(&self.env, frames)?;
        if drifts.is_empty() {
            return Ok(());
        }
        for d in &drifts {
            let file = self.env.profile_env_file(&d.profile)?;
            match d.missing {
                true => eprintln!("envc: '{}' was deleted since it was pushed", d.profile),
                false => eprintln!("envc: {} changed since it was pushed", self.env.tilde(&file)),
            }
            for (label, keys) in [("dangling", &d.dangling), ("changed", &d.changed), ("new", &d.added)] {
                if !keys.is_empty() {
                    eprintln!("envc:     {label:<9}{}", keys.join(" "));
                }
            }
        }
        if yes {
            return Ok(());
        }
        eprint!("envc: continue anyway? [y/N] ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match answer.trim() {
            "y" | "Y" | "yes" => Ok(()),
            _ => Err(anyhow!("aborted; nothing changed")),
        }
    }

    pub fn deliver(&self, what: &str, lines: &[String]) -> Result<()> {
        self.shell.deliver(&self.env, what, lines)
    }
}

pub struct Palette {
    on: bool,
}

impl Palette {
    fn paint(&self, code: &str, s: impl fmt::Display) -> String {
        match self.on {
            true => format!("\x1b[{code}m{s}\x1b[0m"),
            false => s.to_string(),
        }
    }
}

macro_rules! palette_colors {
    ($($name:ident => $code:literal),+ $(,)?) => {
        impl Palette {
            $(pub fn $name(&self, s: impl fmt::Display) -> String {
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

pub trait Loader {
    fn prepare(&self) -> Result<Vec<String>>;
    fn describe(&self) -> Result<String>;
    fn is_on(&self) -> Result<bool>;
    fn apply(&self, profile: &str) -> Result<String>;
    fn revert(&self) -> Result<Option<String>>;
    fn already_off(&self) -> String;
    fn applied_hint(&self, profile: &str) -> Vec<String>;
    fn advice(&self, already_on: bool) -> Result<Vec<String>>;
    fn check_environment(&self);
}

impl Ctx {
    pub fn startup_line(&self, c: &Palette, startup: Option<&str>) -> Result<()> {
        let platform = self.platform();
        let enabled = platform.is_on()?;
        match enabled {
            true => println!("startup loading: {} ({})", c.green("enabled"), platform.describe()?),
            false => println!("startup loading: {}", c.yellow("disabled")),
        }
        match (enabled, startup) {
            (true, Some(name)) => println!("startup profile: {}", c.bold(name)),
            (true, None) => println!("startup profile: (none -- `envc enable <name>`)"),
            (false, Some(name)) => println!(
                "startup profile: {} {}",
                c.bold(name),
                c.dim("(selected, loading off)")
            ),
            (false, None) => println!("startup profile: (none)"),
        }
        if let Some(current) = self.env.active.as_deref() {
            if !(enabled && startup == Some(current)) {
                println!("this shell:      {current}");
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub type Platform<'a> = Posix<'a>;
#[cfg(windows)]
pub type Platform<'a> = Windows<'a>;

#[cfg(not(windows))]
const HOOK: &str = include_str!("text/hook.sh");
#[cfg(not(windows))]
const BEGIN_MARKER: &str = "# >>> envc initialize >>>";
#[cfg(not(windows))]
const END_MARKER: &str = "# <<< envc initialize <<<";

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
}

#[cfg(not(windows))]
impl fmt::Display for ShellKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ShellKind::Bash => "bash",
            ShellKind::Zsh => "zsh",
        })
    }
}

#[cfg(not(windows))]
impl From<Option<&str>> for ShellKind {
    fn from(shell: Option<&str>) -> Self {
        let name = shell
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match () {
            _ if name.contains("zsh") => ShellKind::Zsh,
            _ if name.contains("bash") => ShellKind::Bash,
            _ if cfg!(target_os = "macos") => ShellKind::Zsh,
            _ => ShellKind::Bash,
        }
    }
}

#[cfg(not(windows))]
impl ShellKind {
    fn rc_name(&self) -> &str {
        match self {
            ShellKind::Bash => ".bashrc",
            ShellKind::Zsh => ".zshrc",
        }
    }
}

#[cfg(not(windows))]
pub struct Posix<'a>(&'a Ctx);

#[cfg(not(windows))]
impl<'a> From<&'a Ctx> for Posix<'a> {
    fn from(ctx: &'a Ctx) -> Self {
        Posix(ctx)
    }
}

#[cfg(not(windows))]
impl Posix<'_> {
    fn login_shell(&self) -> ShellKind {
        self.0.env.login_shell.as_deref().into()
    }

    fn rc_file(&self) -> Result<PathBuf> {
        match &self.0.config.rc {
            Some(rc) => Ok(rc.clone()),
            None => Ok(self.0.env.home()?.join(self.login_shell().rc_name())),
        }
    }

    fn rc_content(&self, path: &Path) -> Result<String> {
        match std::fs::read_to_string(path) {
            Ok(c) => Ok(c),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(anyhow!("cannot read {}: {e}", self.0.env.tilde(path))),
        }
    }

    fn hook_installed(&self) -> Result<bool> {
        Ok(self.rc_content(&self.rc_file()?)?.contains(BEGIN_MARKER))
    }

    fn write_rc(&self, path: &Path, content: String) -> Result<()> {
        std::fs::write(path, content).with_context(|| format!("cannot write {}", self.0.env.tilde(path)))
    }

    fn install_hook(&self) -> Result<bool> {
        let path = self.rc_file()?;
        let (mut updated, had_block) = Posix::strip_block(&self.rc_content(&path)?);
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
        updated.push_str(HOOK);
        self.write_rc(&path, updated)?;
        Ok(had_block)
    }

    fn remove_hook(&self) -> Result<bool> {
        let path = self.rc_file()?;
        let (stripped, had_block) = Posix::strip_block(&self.rc_content(&path)?);
        if had_block {
            self.write_rc(&path, stripped)?;
        }
        Ok(had_block)
    }

    fn strip_block(content: &str) -> (String, bool) {
        let mut out = String::new();
        let mut inside = false;
        let mut found = false;
        for line in content.lines() {
            match line.trim_end() {
                BEGIN_MARKER => {
                    inside = true;
                    found = true;
                    if out.ends_with("\n\n") {
                        out.pop();
                    }
                }
                END_MARKER => inside = false,
                _ if !inside => {
                    out.push_str(line);
                    out.push('\n');
                }
                _ => {}
            }
        }
        if !content.is_empty() && !content.ends_with('\n') {
            out.pop();
        }
        (out, found)
    }

    fn binary_on_path(&self) -> bool {
        self.0
            .env
            .path
            .as_ref()
            .is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join("envc").is_file()))
    }

    fn suggested_install_path(&self) -> String {
        let dir = match (cfg!(target_os = "macos"), self.0.env.home()) {
            (false, Ok(home)) => home.join(".local/bin"),
            _ => PathBuf::from("/usr/local/bin"),
        };
        dir.join("envc").display().to_string()
    }
}

#[cfg(not(windows))]
impl Loader for Posix<'_> {
    fn prepare(&self) -> Result<Vec<String>> {
        let rc = self.rc_file()?;
        let already = self.hook_installed()?;
        if !already {
            self.install_hook()?;
        }
        let source = match self.0.env.login_shell.as_deref() {
            Some(shell) => format!("$SHELL={shell}"),
            None => format!("the {} default", std::env::consts::OS),
        };
        Ok(vec![
            format!("shell:   {} ({source})", self.login_shell()),
            format!("rc file: {}", self.0.env.tilde(&rc)),
            format!("hook:    {}", if already { "already installed" } else { "installed" }),
        ])
    }

    fn describe(&self) -> Result<String> {
        Ok(self.0.env.tilde(&self.rc_file()?).to_string())
    }

    fn is_on(&self) -> Result<bool> {
        self.hook_installed()
    }

    fn apply(&self, _profile: &str) -> Result<String> {
        let rc = self.rc_file()?;
        let rc = self.0.env.tilde(&rc);
        Ok(match self.install_hook()? {
            false => format!("installed the hook into {rc}"),
            true => format!("refreshed the hook in {rc}"),
        })
    }

    fn revert(&self) -> Result<Option<String>> {
        if !self.remove_hook()? {
            return Ok(None);
        }
        Ok(Some(format!("removed the hook from {}", self.0.env.tilde(&self.rc_file()?))))
    }

    fn already_off(&self) -> String {
        match self.rc_file() {
            Ok(rc) => format!(
                "startup loading was already disabled ({} has no hook)",
                self.0.env.tilde(&rc)
            ),
            Err(_) => "startup loading was already disabled".to_string(),
        }
    }

    fn applied_hint(&self, profile: &str) -> Vec<String> {
        vec![format!("new shells load it; `envc use {profile}` for this one")]
    }

    fn advice(&self, already_on: bool) -> Result<Vec<String>> {
        if already_on {
            return Ok(Vec::new());
        }
        Ok(vec![format!(
            "open a new shell, or `source {}`.",
            self.0.env.tilde(&self.rc_file()?)
        )])
    }

    fn check_environment(&self) {
        if self.binary_on_path() {
            return;
        }
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "envc".to_string());
        eprintln!();
        eprintln!("envc: warning: not on PATH, so the hook cannot fire in new shells.");
        eprintln!("envc:          install -Dm755 {exe} {}", self.suggested_install_path());
    }
}

#[cfg(windows)]
pub struct Windows<'a>(&'a Ctx);

#[cfg(windows)]
impl<'a> From<&'a Ctx> for Windows<'a> {
    fn from(ctx: &'a Ctx) -> Self {
        Windows(ctx)
    }
}

#[cfg(windows)]
impl Windows<'_> {
    fn powershell(&self, script: &str) -> Result<String> {
        let mut candidates = vec![PathBuf::from("powershell")];
        if let Some(root) = &self.0.env.system_root {
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
            if !out.status.success() {
                return Err(anyhow!(
                    "powershell failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            return Ok(String::from_utf8_lossy(&out.stdout).to_string());
        }
        Err(anyhow!("cannot run powershell ({})", spawn_error.unwrap_or_default()))
    }

    fn read_user_vars(&self, keys: &[&str]) -> Result<HashMap<String, Option<String>>> {
        let list = keys.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        let script = format!(
            "{list} | ForEach-Object {{ $v = [Environment]::GetEnvironmentVariable($_,'User'); \
             if ($null -eq $v) {{ $_ }} else {{ \"$_=$v\" }} }}"
        );
        Ok(self
            .powershell(&script)?
            .lines()
            .map(|line| match line.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), Some(v.to_string())),
                None => (line.trim().to_string(), None),
            })
            .collect())
    }

    fn write_user_vars(&self, pairs: &[(&str, Option<&str>)]) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        let script = pairs
            .iter()
            .map(|(k, v)| {
                let v = v.map(Shell::quote_powershell).unwrap_or_else(|| "$null".to_string());
                format!("[Environment]::SetEnvironmentVariable('{k}',{v},'User')")
            })
            .collect::<Vec<_>>()
            .join("; ");
        self.powershell(&script)?;
        Ok(())
    }
}

#[cfg(windows)]
impl Loader for Windows<'_> {
    fn prepare(&self) -> Result<Vec<String>> {
        Ok(vec![
            format!("shell:   {}", Shell::detect(&self.0.env)),
            "startup: user environment (no rc file to patch)".to_string(),
        ])
    }

    fn describe(&self) -> Result<String> {
        Ok("user environment".to_string())
    }

    fn is_on(&self) -> Result<bool> {
        Ok(UserStack::load(&self.0.env)?.is_some())
    }

    fn apply(&self, profile: &str) -> Result<String> {
        let env = &self.0.env;
        let assignments = Assignment::load(env, &env.profile_env_file(profile)?, &env.vars)?;
        if assignments.is_empty() {
            return Err(anyhow!("profile '{profile}' sets no variables"));
        }

        let keys: Vec<&str> = assignments.iter().map(|a| a.key.as_str()).collect();
        let before: HashMap<String, String> = self
            .read_user_vars(&keys)?
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();
        let frame = Frame::capture(profile, &before, &assignments, None);

        let pairs: Vec<(&str, Option<&str>)> =
            assignments.iter().map(|a| (a.key.as_str(), Some(a.value.as_str()))).collect();
        self.write_user_vars(&pairs)?;
        UserStack(frame).save(env)?;

        Ok(format!("wrote {} variables to your user environment", assignments.len()))
    }

    fn revert(&self) -> Result<Option<String>> {
        let Some(UserStack(frame)) = UserStack::load(&self.0.env)? else {
            return Ok(None);
        };
        let pairs: Vec<(&str, Option<&str>)> =
            frame.entries.iter().map(|e| (e.key.as_str(), e.prev.as_deref())).collect();
        self.write_user_vars(&pairs)?;
        UserStack::remove(&self.0.env)?;
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

    fn advice(&self, _already_on: bool) -> Result<Vec<String>> {
        if self.0.quiet {
            return Ok(Vec::new());
        }
        Ok(vec![
            "to apply a profile to just this shell:".to_string(),
            "    cmd        envc use work && call \"%TEMP%\\envc\\use.cmd\"".to_string(),
            "    powershell envc use work | Invoke-Expression".to_string(),
        ])
    }

    fn check_environment(&self) {}
}
