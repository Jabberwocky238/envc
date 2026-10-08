#![cfg(target_os = "windows")]

mod general;

use std::fs;
use std::process::{Command, Output};

use general::{stdout_of, Checks, Cmd, Lab, PowerShell, ENVC};

struct Win {
    lab: Lab,
    prefix: String,
}

impl std::ops::Deref for Win {
    type Target = Lab;

    fn deref(&self) -> &Lab {
        &self.lab
    }
}

impl Win {
    fn new() -> Win {
        let lab = Lab::new("win");
        let prefix = format!(
            "ENVC_T_{}_{}_",
            std::process::id(),
            lab.root.file_name().unwrap().to_string_lossy().rsplit('-').next().unwrap()
        );
        Win { lab, prefix }
    }

    fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    fn powershell(&self, script: &str) -> String {
        let out = self
            .command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .expect("powershell should start");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn cmd(&self, script: &str) -> String {
        stdout_of(&self.script(&Cmd, script))
    }

    fn user_var(&self, key: &str) -> Option<String> {
        let got = self.powershell(&format!(
            "$v = [Environment]::GetEnvironmentVariable('{key}','User'); \
             if ($null -eq $v) {{ '' }} else {{ $v }}"
        ));
        (!got.is_empty()).then_some(got)
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        for name in ["FOO", "BAR", "PRE_EXISTING"] {
            let key = self.key(name);
            let _ = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("[Environment]::SetEnvironmentVariable('{key}',$null,'User')"),
                ])
                .output();
        }
    }
}

fn describe(out: &Output) -> String {
    format!(
        "exit={:?}\n     stdout: {}\n     stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim_end(),
        String::from_utf8_lossy(&out.stderr).trim_end()
    )
}

#[test]
fn overlap_in_powershell() {
    general::overlap(&Lab::new("win"), &PowerShell);
}

#[test]
fn overlap_in_cmd() {
    general::overlap(&Lab::new("win"), &Cmd);
}

#[test]
fn two_powershell_sessions_keep_their_own_stacks() {
    general::cross_sessions(&Lab::new("win"), &PowerShell);
}

#[test]
fn two_cmd_sessions_keep_their_own_stacks() {
    general::cross_sessions(&Lab::new("win"), &Cmd);
}

#[test]
fn a_changed_profile_asks_before_use() {
    general::drift(&Lab::new("win"));
}

#[test]
fn unuse_names_what_it_pops() {
    general::unuse_needs_a_name_on_the_stack(&Lab::new("win"));
}

#[test]
fn a_stale_stack_is_ignored() {
    general::a_stale_stack_is_ignored(&Lab::new("win"));
}

#[test]
fn settings_come_from_the_config_file() {
    general::config_file(&Lab::new("win"));
}

#[test]
fn enable_writes_user_level_variables() {
    let lab = Win::new();
    let (foo, bar) = (lab.key("FOO"), lab.key("BAR"));
    lab.profile("work", &format!("{foo}=from-work\n{bar}=$HOME\n"));
    let out = lab.run(&["enable", "work"]);

    let mut c = Checks::default();
    c.is_true(&format!("enable succeeds:\n     {}", describe(&out)), out.status.success());
    c.eq("written to the user environment", "from-work", &lab.user_var(&foo).unwrap_or_default());
    c.is_true("the choice is kept in ~/.envc/startup", !lab.read("startup").trim().is_empty());
    c.is_true("the old values are kept for disable", !lab.read("user-stack").is_empty());
    c.done();
}

#[test]
fn disable_puts_the_user_variables_back() {
    let lab = Win::new();
    let (foo, bar) = (lab.key("FOO"), lab.key("BAR"));
    lab.profile("work", &format!("{foo}=from-work\n{bar}=from-work\n"));
    lab.run(&["enable", "work"]);
    lab.run(&["disable", "work"]);

    let mut c = Checks::default();
    for key in [&foo, &bar] {
        c.is_true(&format!("{key} is removed"), lab.user_var(key).is_none());
    }
    c.is_true("disable removes the user-stack", lab.read("user-stack").trim().is_empty());
    c.eq("disable clears the choice", "", &lab.read("startup"));
    c.done();
}

#[test]
fn disable_restores_a_value_that_was_already_there() {
    let lab = Win::new();
    let key = lab.key("PRE_EXISTING");
    lab.profile("work", &format!("{key}=from-work\n"));
    lab.powershell(&format!("[Environment]::SetEnvironmentVariable('{key}','original','User')"));

    lab.run(&["enable", "work"]);
    let during = lab.user_var(&key);
    lab.run(&["disable", "work"]);
    let after = lab.user_var(&key);

    let mut c = Checks::default();
    c.eq("enable overrides the old value", "from-work", &during.unwrap_or_default());
    c.eq("disable restores original", "original", &after.unwrap_or_default());
    c.done();
}

#[test]
fn init_does_not_touch_a_shell_profile() {
    let lab = Win::new();
    let profile = lab.home().join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1");
    let out = lab.run(&["init"]);

    let mut c = Checks::default();
    c.is_true("init succeeds", out.status.success());
    c.has("startup loading goes through the user environment", "user environment", &stdout_of(&out));
    c.is_true("the PowerShell profile is not touched", !profile.exists());
    c.done();
}

#[test]
fn cmd_runs_the_batch_file_it_writes() {
    let lab = Win::new();
    let foo = lab.key("FOO");
    lab.profile("work", &format!("{foo}=from work\n"));
    let out = lab.run(&["--shell", "cmd", "use", "work"]);
    let path = lab.batch("use");

    let mut c = Checks::default();
    c.eq("cmd gets nothing on stdout", "", &stdout_of(&out));
    c.is_true(&format!("{} is written", path.display()), path.is_file());
    c.eq(
        "the variable is set after call",
        "from work",
        &lab.cmd(&format!("call \"{}\" && echo !{foo}!", path.display())),
    );
    c.done();
}

#[test]
fn the_shell_is_detected_from_the_environment() {
    let lab = Win::new();
    lab.profile("work", "FOO=bar\n");
    let ps = lab.command(ENVC).args(["use", "work"]).env("PSModulePath", r"C:\Modules").output().unwrap();
    let cmd = lab.command(ENVC).args(["use", "work"]).env_remove("PSModulePath").output().unwrap();

    let mut c = Checks::default();
    c.has("with PSModulePath it emits PowerShell", "$env:", &stdout_of(&ps));
    c.eq("without PSModulePath it writes a batch file", "", &stdout_of(&cmd));
    c.done();
}

#[test]
fn the_stack_lives_under_the_session() {
    let lab = Win::new();
    lab.profile("work", "FOO=bar\n");
    let out = lab.script(&PowerShell, &format!("& '{ENVC}' --shell powershell use work | Invoke-Expression\n& '{ENVC}' stack"));
    let mut c = Checks::default();
    c.has("envc stack recognises this PowerShell session", "base > work", &stdout_of(&out));
    c.is_true("the stack file lives in the stack directory", lab.envc_home().join("stack").is_dir());
    c.is_true("the stack directory is readable", fs::read_dir(lab.envc_home().join("stack")).is_ok());
    c.done();
}
