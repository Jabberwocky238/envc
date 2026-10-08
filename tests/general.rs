#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

pub const ENVC: &str = env!("CARGO_BIN_EXE_envc");

pub struct Lab {
    pub root: PathBuf,
}

impl Lab {
    pub fn new(tag: &str) -> Lab {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "envc-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(ENVC, root.join("bin/envc")).unwrap();
        Lab { root }
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn envc_home(&self) -> PathBuf {
        self.home().join(".envc")
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn batch(&self, what: &str) -> PathBuf {
        self.tmp().join("envc").join(format!("{what}.cmd"))
    }

    #[cfg(unix)]
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut path = vec![self.root.join("bin")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        vec![
            ("HOME", self.home().into()),
            ("PATH", std::env::join_paths(path).unwrap()),
            ("TMPDIR", self.tmp().into()),
        ]
    }

    #[cfg(windows)]
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("USERPROFILE", self.home().into()),
            ("HOME", self.home().into()),
            ("TEMP", self.tmp().into()),
            ("TMP", self.tmp().into()),
        ]
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        #[cfg(unix)]
        cmd.env_clear();
        cmd.envs(self.env()).env_remove("ENVC_ACTIVE").stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(ENVC).args(args).output().expect("envc should start")
    }

    pub fn run_as(&self, active: &str, args: &[&str]) -> Output {
        self.command(ENVC)
            .args(args)
            .env("ENVC_ACTIVE", active)
            .output()
            .expect("envc should start")
    }

    pub fn profile(&self, name: &str, body: &str) {
        let dir = self.envc_home().join("profiles").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".env"), body).unwrap();
    }

    pub fn profile_path(&self, name: &str) -> PathBuf {
        self.envc_home().join("profiles").join(name).join(".env")
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.envc_home().join(rel)).unwrap_or_default()
    }

    pub fn stacks(&self) -> Vec<String> {
        let Ok(dir) = fs::read_dir(self.envc_home().join("stack")) else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok())
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .collect()
    }

    pub fn flag(&self, name: &str) -> PathBuf {
        self.tmp().join(name)
    }

    fn script_command(&self, dialect: &dyn Dialect, body: &str) -> Command {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = self.tmp().join(format!(
            "script-{}.{}",
            N.fetch_add(1, Ordering::Relaxed),
            dialect.extension()
        ));
        fs::write(&path, dialect.wrap(body)).unwrap();
        let (program, args) = dialect.launch(&path);
        let mut cmd = self.command(program);
        cmd.args(args);
        #[cfg(unix)]
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                libc::setsid();
                Ok(())
            });
        }
        cmd
    }

    pub fn script(&self, dialect: &dyn Dialect, body: &str) -> Output {
        self.script_command(dialect, body).output().expect("the shell should start")
    }

    pub fn spawn(&self, dialect: &dyn Dialect, body: &str) -> Child {
        self.script_command(dialect, body)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the shell should start")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
pub struct Checks {
    failed: Vec<String>,
}

impl Checks {
    pub fn eq(&mut self, what: &str, expected: &str, actual: &str) -> &mut Self {
        if expected != actual {
            self.failed.push(format!(
                "{what}\n       expected: {expected:?}\n       actual:   {actual:?}"
            ));
        }
        self
    }

    pub fn has(&mut self, what: &str, needle: &str, hay: &str) -> &mut Self {
        if !hay.contains(needle) {
            self.failed.push(format!("{what}\n       {needle:?} not found in:\n{hay}"));
        }
        self
    }

    pub fn lacks(&mut self, what: &str, needle: &str, hay: &str) -> &mut Self {
        if hay.contains(needle) {
            self.failed.push(format!("{what}\n       {needle:?} should not be in:\n{hay}"));
        }
        self
    }

    pub fn is_true(&mut self, what: &str, yes: bool) -> &mut Self {
        if !yes {
            self.failed.push(what.to_string());
        }
        self
    }

    pub fn done(&mut self) {
        assert!(
            self.failed.is_empty(),
            "\n{} failed:\n{}",
            self.failed.len(),
            self.failed.join("\n")
        );
    }
}

pub fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n").trim_end().to_string()
}

pub fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n").trim_end().to_string()
}

pub fn lines_of(out: &Output) -> String {
    stdout_of(out).lines().map(str::trim_end).collect::<Vec<_>>().join("|")
}

pub trait Dialect {
    fn extension(&self) -> &str;
    fn launch(&self, script: &Path) -> (OsString, Vec<OsString>);
    fn wrap(&self, body: &str) -> String {
        body.to_string()
    }
    fn set(&self, key: &str, value: &str) -> String;
    fn unset(&self, key: &str) -> String;
    fn envc(&self, args: &str) -> String;
    fn show(&self, keys: &[&str]) -> String;
    fn wait_for(&self, flag: &Path) -> String;
    fn touch(&self, flag: &Path) -> String;

    fn lines(&self, steps: &[String]) -> String {
        steps.join("\n")
    }
}

pub struct Posix {
    pub program: &'static str,
}

impl Dialect for Posix {
    fn extension(&self) -> &str {
        "sh"
    }

    fn launch(&self, script: &Path) -> (OsString, Vec<OsString>) {
        (self.program.into(), vec![script.into()])
    }

    fn set(&self, key: &str, value: &str) -> String {
        format!("export {key}='{value}'")
    }

    fn unset(&self, key: &str) -> String {
        format!("unset {key}")
    }

    fn envc(&self, args: &str) -> String {
        format!("eval \"$('{ENVC}' {args})\"")
    }

    fn show(&self, keys: &[&str]) -> String {
        keys.iter()
            .map(|k| format!("printf '%s\\n' \"${{{k}-<unset>}}\""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for(&self, flag: &Path) -> String {
        format!("while [ ! -e '{}' ]; do sleep 0.05; done", flag.display())
    }

    fn touch(&self, flag: &Path) -> String {
        format!(": > '{}'", flag.display())
    }
}

pub struct PowerShell;

impl Dialect for PowerShell {
    fn extension(&self) -> &str {
        "ps1"
    }

    fn launch(&self, script: &Path) -> (OsString, Vec<OsString>) {
        (
            "powershell".into(),
            ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]
                .iter()
                .map(OsString::from)
                .chain([script.into()])
                .collect(),
        )
    }

    fn set(&self, key: &str, value: &str) -> String {
        format!("$env:{key}='{value}'")
    }

    fn unset(&self, key: &str) -> String {
        format!("Remove-Item Env:{key} -ErrorAction SilentlyContinue")
    }

    fn envc(&self, args: &str) -> String {
        format!("& '{ENVC}' --shell powershell {args} | Invoke-Expression")
    }

    fn show(&self, keys: &[&str]) -> String {
        keys.iter()
            .map(|k| format!("if ($null -eq $env:{k}) {{ Write-Output '<unset>' }} else {{ Write-Output $env:{k} }}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for(&self, flag: &Path) -> String {
        format!(
            "while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 50 }}",
            flag.display()
        )
    }

    fn touch(&self, flag: &Path) -> String {
        format!("New-Item -ItemType File '{}' | Out-Null", flag.display())
    }
}

pub struct Cmd;

impl Cmd {
    fn what(args: &str) -> &str {
        args.split_whitespace().find(|w| !w.starts_with('-')).unwrap_or("use")
    }
}

impl Dialect for Cmd {
    fn extension(&self) -> &str {
        "cmd"
    }

    fn launch(&self, script: &Path) -> (OsString, Vec<OsString>) {
        ("cmd".into(), vec!["/v:on".into(), "/c".into(), script.into()])
    }

    fn wrap(&self, body: &str) -> String {
        format!("@echo off\r\n{}\r\n", body.replace('\n', "\r\n"))
    }

    fn set(&self, key: &str, value: &str) -> String {
        format!("set \"{key}={value}\"")
    }

    fn unset(&self, key: &str) -> String {
        format!("set \"{key}=\"")
    }

    fn envc(&self, args: &str) -> String {
        format!(
            "\"{ENVC}\" --shell cmd {args} && call \"%TEMP%\\envc\\{}.cmd\"",
            Cmd::what(args)
        )
    }

    fn show(&self, keys: &[&str]) -> String {
        keys.iter()
            .map(|k| format!("if defined {k} (echo !{k}!) else (echo ^<unset^>)"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for(&self, flag: &Path) -> String {
        format!(
            ":wait_{n}\nif not exist \"{p}\" (ping -n 1 127.0.0.1 >nul & goto wait_{n})",
            n = flag.file_name().unwrap().to_string_lossy().replace(['.', '-'], "_"),
            p = flag.display()
        )
    }

    fn touch(&self, flag: &Path) -> String {
        format!("type nul > \"{}\"", flag.display())
    }
}

pub fn overlap_profiles(lab: &Lab) {
    lab.profile("a", "EV_SHARED=from-a\nEV_ONLY_A=a\n");
    lab.profile("b", "EV_SHARED=from-b\nEV_ONLY_B=b-$EV_ONLY_A\n");
}

const KEYS: [&str; 4] = ["EV_SHARED", "EV_ONLY_A", "EV_ONLY_B", "ENVC_ACTIVE"];

pub fn overlap(lab: &Lab, d: &dyn Dialect) {
    overlap_profiles(lab);
    let out = lab.script(
        d,
        &d.lines(&[
            d.set("EV_SHARED", "base"),
            d.unset("EV_ONLY_A"),
            d.unset("EV_ONLY_B"),
            d.envc("use a"),
            d.envc("use b"),
            d.show(&KEYS),
            d.envc("unuse a"),
            d.show(&KEYS),
            d.envc("unuse b"),
            d.show(&KEYS),
        ]),
    );

    let mut c = Checks::default();
    c.eq(
        "use a -> use b -> unuse a -> unuse b",
        "from-b|a|b-a|a:b|from-b|<unset>|b-|b|base|<unset>|<unset>|<unset>",
        &lines_of(&out),
    );
    c.lacks("no errors", "error", &stderr_of(&out).to_lowercase());
    c.is_true("an empty stack leaves no file", lab.stacks().is_empty());

    let history = lab.read("history");
    let actions: Vec<&str> = history
        .lines()
        .filter(|l| l.starts_with("@@ "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    c.eq("the history records every push/pop/repush", "push push pop pop repush pop", &actions.join(" "));
    c.has("the history records the restore snapshot", "-EV_SHARED=base", &history);
    c.has("the history records the session", "session=", &history);
    for name in ["a", "b"] {
        let snaps: Vec<_> = fs::read_dir(lab.envc_home().join("snapshots").join(name))
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        c.eq(&format!("{name} has exactly one snapshot"), "1", &snaps.len().to_string());
        c.eq(
            &format!("the {name} snapshot is the .env that was used"),
            &fs::read_to_string(lab.profile_path(name)).unwrap(),
            &snaps.first().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default(),
        );
    }
    c.done();
}

pub fn cross_sessions(lab: &Lab, d: &dyn Dialect) {
    overlap_profiles(lab);
    let (s1_ready, s2_done) = (lab.flag("s1-ready"), lab.flag("s2-done"));

    let first = lab.spawn(
        d,
        &d.lines(&[
            d.set("EV_SHARED", "one"),
            d.envc("use a"),
            d.touch(&s1_ready),
            d.wait_for(&s2_done),
            d.show(&["EV_SHARED", "ENVC_ACTIVE"]),
            d.envc("unuse a"),
            d.show(&["EV_SHARED", "ENVC_ACTIVE"]),
        ]),
    );
    let second = lab.script(
        d,
        &d.lines(&[
            d.set("EV_SHARED", "two"),
            d.unset("EV_ONLY_B"),
            d.wait_for(&s1_ready),
            d.envc("use b"),
            d.envc("use a"),
            d.envc("unuse b"),
            d.show(&["EV_SHARED", "EV_ONLY_B", "ENVC_ACTIVE"]),
            d.touch(&s2_done),
        ]),
    );
    let stacks_while_both = lab.stacks().len();
    let first = first.wait_with_output().unwrap();

    let mut c = Checks::default();
    c.eq("the second shell's own stack: b, a, -b", "from-a|<unset>|a", &lines_of(&second));
    c.eq("the first shell is not affected by the second", "from-a|a|one|<unset>", &lines_of(&first));
    c.eq("each shell has its own stack file", "2", &stacks_while_both.to_string());
    c.lacks("no errors in the first shell", "error", &stderr_of(&first).to_lowercase());
    c.lacks("no errors in the second shell", "error", &stderr_of(&second).to_lowercase());
    c.done();
}

pub fn drift(lab: &Lab) {
    overlap_profiles(lab);
    let mut c = Checks::default();

    c.is_true("use a", lab.run(&["use", "a"]).status.success());
    fs::write(lab.profile_path("a"), "EV_SHARED=changed\nEV_NEW=1\n").unwrap();

    let refused = lab.run_as("a", &["use", "b"]);
    let err = stderr_of(&refused);
    c.is_true("without y there is no use", !refused.status.success());
    c.eq("a refusal prints no shell code", "", &stdout_of(&refused));
    c.has("it names the changed profile", "a", &err);
    c.has("it names the dangling variable", "dangling EV_ONLY_A", &err);
    c.has("it names the changed variable", "changed  EV_SHARED", &err);
    c.has("it names the new variable", "new      EV_NEW", &err);
    c.has("it asks for y", "[y/N]", &err);
    c.lacks("the stack is untouched", "@@ b", &lab.stacks().concat());

    let mut child = lab
        .command(ENVC)
        .args(["use", "b"])
        .env("ENVC_ACTIVE", "a")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"y\n").unwrap();
    let agreed = child.wait_with_output().unwrap();
    c.is_true("y goes ahead", agreed.status.success());
    c.has("b was pushed", "@@ b", &lab.stacks().concat());

    let popped = lab.run_as("a:b", &["unuse", "b"]);
    c.is_true("unuse of the top frame does not re-read the frames below", popped.status.success());
    c.lacks("so it does not ask", "[y/N]", &stderr_of(&popped));

    let forced = lab.run_as("a", &["use", "-y", "b"]);
    c.is_true("-y skips the question", forced.status.success());
    c.lacks("-y does not ask", "[y/N]", &stderr_of(&forced));
    c.done();
}

pub fn unuse_needs_a_name_on_the_stack(lab: &Lab) {
    overlap_profiles(lab);
    let mut c = Checks::default();
    c.is_true("unuse requires a name", !lab.run(&["unuse"]).status.success());
    c.is_true("unuse on an empty stack fails", !lab.run(&["unuse", "a"]).status.success());
    lab.run(&["use", "a"]);
    let out = lab.run_as("a", &["unuse", "b"]);
    c.is_true("a profile that is not on the stack cannot be unused", !out.status.success());
    c.has("and shows what is on the stack", "base > a", &stderr_of(&out));
    let again = lab.run_as("a", &["use", "a"]);
    c.is_true("the same profile cannot be pushed twice", !again.status.success());
    c.has("and points at unuse", "envc unuse a", &stderr_of(&again));
    c.is_true("push/pop are aliases", lab.run_as("a", &["pop", "a"]).status.success());
    c.done();
}

pub fn a_stale_stack_is_ignored(lab: &Lab) {
    overlap_profiles(lab);
    let mut c = Checks::default();
    lab.run(&["use", "a"]);
    let fresh = lab.run(&["use", "b"]);
    c.is_true("a new session (no ENVC_ACTIVE) does not inherit an old stack", fresh.status.success());
    c.has("it starts from base", "base > b", &stderr_of(&fresh));
    let other = lab.run_as("x", &["use", "a"]);
    c.has("a stack that does not match this shell is warned about", "warning", &stderr_of(&other));
    c.done();
}

pub fn config_file(lab: &Lab) {
    overlap_profiles(lab);
    let mut c = Checks::default();
    fs::create_dir_all(lab.envc_home()).unwrap();

    fs::write(lab.envc_home().join("config"), "shell = powershell\ncolor = always\n").unwrap();
    c.has("config picks the shell", "$env:EV_SHARED='from-a'", &stdout_of(&lab.run(&["use", "a"])));
    c.has("config picks the color", "\x1b[", &stdout_of(&lab.run(&["list"])));

    fs::write(lab.envc_home().join("config"), "colour = never\n").unwrap();
    let bad = lab.run(&["list"]);
    c.is_true("an unknown setting fails", !bad.status.success());
    c.has("and names the line", "config:1", &stderr_of(&bad));
    c.done();
}
