#![cfg(target_os = "macos")]

mod general;

use std::fs;
use std::process::Output;

use general::{Checks, Lab, Posix};

const ZSH: Posix = Posix { program: "zsh" };

struct Zsh {
    lab: Lab,
    shell: Option<&'static str>,
}

impl std::ops::Deref for Zsh {
    type Target = Lab;

    fn deref(&self) -> &Lab {
        &self.lab
    }
}

impl Zsh {
    fn new(shell: Option<&'static str>) -> Zsh {
        Zsh {
            lab: Lab::new("macos"),
            shell,
        }
    }

    fn zshrc(&self) -> std::path::PathBuf {
        self.home().join(".zshrc")
    }

    fn command(&self, program: &str) -> std::process::Command {
        let mut cmd = self.lab.command(program);
        if let Some(shell) = self.shell {
            cmd.env("SHELL", shell);
        }
        cmd
    }

    fn envc(&self, args: &[&str]) -> Output {
        self.command(general::ENVC).args(args).output().expect("envc should start")
    }

    fn zsh(&self, script: &str) -> String {
        let out = self.command("zsh").arg("-c").arg(script).output().expect("zsh should start");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn sourced(&self, script: &str) -> String {
        self.zsh(&format!("source \"{}\"; {script}", self.zshrc().display()))
    }

    fn two_profiles(&self) {
        self.profile("work", "EDITOR=vim\nPROJECT=work\n");
        self.profile("alt", "EDITOR=emacs\nPROJECT=alt\n");
    }
}

#[test]
fn overlap_in_zsh() {
    general::overlap(&Lab::new("macos"), &ZSH);
}

#[test]
fn two_zsh_sessions_keep_their_own_stacks() {
    general::cross_sessions(&Lab::new("macos"), &ZSH);
}

#[test]
fn a_changed_profile_asks_before_use() {
    general::drift(&Lab::new("macos"));
}

#[test]
fn unuse_names_what_it_pops() {
    general::unuse_needs_a_name_on_the_stack(&Lab::new("macos"));
}

#[test]
fn a_stale_stack_is_ignored() {
    general::a_stale_stack_is_ignored(&Lab::new("macos"));
}

#[test]
fn settings_come_from_the_config_file() {
    general::config_file(&Lab::new("macos"));
}

#[test]
fn a_zsh_login_shell_gets_the_hook_in_zshrc() {
    let lab = Zsh::new(Some("/bin/zsh"));
    let report = String::from_utf8_lossy(&lab.envc(&["init"]).stdout).to_string();
    let rc = fs::read_to_string(lab.zshrc()).unwrap_or_default();

    let mut c = Checks::default();
    c.has("the report says zsh", "shell:   zsh", &report);
    c.has("the report points at .zshrc", ".zshrc", &report);
    c.has("the hook goes into ~/.zshrc", "# >>> envc initialize >>>", &rc);
    c.done();
}

#[test]
fn without_a_shell_variable_macos_defaults_to_zsh() {
    let lab = Zsh::new(None);
    lab.envc(&["init"]);
    let mut c = Checks::default();
    c.has(
        "without $SHELL, macOS defaults to zsh",
        "# >>> envc initialize >>>",
        &fs::read_to_string(lab.zshrc()).unwrap_or_default(),
    );
    c.is_true(".bashrc is not written as well", !lab.home().join(".bashrc").exists());
    c.done();
}

#[test]
fn the_hook_is_valid_zsh_syntax() {
    let lab = Zsh::new(Some("/bin/zsh"));
    lab.envc(&["init"]);
    let out = lab.command("zsh").arg("-n").arg(lab.zshrc()).output().expect("zsh should start");
    Checks::default()
        .is_true(
            &format!("the hook is valid zsh:\n{}", String::from_utf8_lossy(&out.stderr)),
            out.status.success(),
        )
        .done();
}

#[test]
fn use_and_unuse_round_trip_in_zsh() {
    let lab = Zsh::new(Some("/bin/zsh"));
    lab.two_profiles();
    let out = lab.zsh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT
eval "$(envc use work)"
printf "on=%s,%s " "$EDITOR" "$PROJECT"
eval "$(envc unuse work)"
printf "off=%s,%s" "$EDITOR" "${PROJECT-<unset>}""#,
    );
    Checks::default().eq("use/unuse round trip in zsh", "on=vim,work off=nano,<unset>", &out).done();
}

#[test]
fn the_wrapper_works_in_zsh() {
    let lab = Zsh::new(Some("/bin/zsh"));
    lab.two_profiles();
    lab.envc(&["init"]);
    let out = lab.sourced(
        r#"unset PROJECT; envc use alt 2>/dev/null; printf "%s " "$PROJECT"; envc pop alt 2>/dev/null; printf "%s" "${PROJECT-<unset>}""#,
    );
    Checks::default().eq("the zsh hook wraps use/pop", "alt <unset>", &out).done();
}

#[test]
fn a_new_zsh_autoloads_the_startup_profile() {
    let lab = Zsh::new(Some("/bin/zsh"));
    lab.two_profiles();
    lab.envc(&["init"]);
    lab.envc(&["enable", "work"]);
    let out = lab.sourced(r#"printf "%s|%s" "$PROJECT" "$ENVC_ACTIVE""#);
    Checks::default().eq("a new zsh autoloads", "work|work", &out).done();
}

#[test]
fn the_two_halves_stay_independent_in_zsh() {
    let lab = Zsh::new(Some("/bin/zsh"));
    lab.two_profiles();
    lab.envc(&["init"]);
    lab.envc(&["enable", "work"]);

    let mut c = Checks::default();
    c.eq(
        "use changes the current zsh",
        "alt",
        &lab.sourced(r#"envc use alt >/dev/null 2>&1; printf "%s" "$PROJECT""#),
    );
    c.has("the startup choice is still work", "work", &lab.read("startup"));
    c.eq("a new zsh still loads work", "work", &lab.sourced(r#"printf "%s" "$PROJECT""#));
    c.done();
}
