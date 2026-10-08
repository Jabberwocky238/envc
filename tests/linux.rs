#![cfg(target_os = "linux")]

mod general;

use std::fs;

use general::{stderr_of, stdout_of, Checks, Lab, Posix, ENVC};

const BASH: Posix = Posix { program: "bash" };

struct Bash(Lab);

impl std::ops::Deref for Bash {
    type Target = Lab;

    fn deref(&self) -> &Lab {
        &self.0
    }
}

impl Bash {
    fn new() -> Bash {
        let lab = Bash(Lab::new("linux"));
        fs::write(lab.rc(), "export KEEPME=1\n").unwrap();
        lab
    }

    fn rc(&self) -> std::path::PathBuf {
        self.home().join(".bashrc")
    }

    fn bash(&self, script: &str, sourced: bool) -> String {
        let prefix = match sourced {
            true => format!("source \"{}\"; ", self.rc().display()),
            false => String::new(),
        };
        let out = self
            .command("bash")
            .env("SHELL", "/bin/bash")
            .arg("-c")
            .arg(format!("{prefix}{script}"))
            .output()
            .expect("bash should start");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn sh(&self, script: &str) -> String {
        self.bash(script, false)
    }

    fn sourced(&self, script: &str) -> String {
        self.bash(script, true)
    }

    fn two_profiles(&self) {
        self.profile(
            "work",
            "# the profile used by most tests\n\
             EDITOR=vim\n\
             PROJECT=work\n\
             TOKEN=\"a b\"\n\
             PATH=\"$HOME/bin:$PATH\"\n\
             export EXPLICIT=yes\n",
        );
        self.profile("alt", "EDITOR=emacs\nPROJECT=alt\n");
    }
}

#[test]
fn overlap_in_bash() {
    general::overlap(&Lab::new("linux"), &BASH);
}

#[test]
fn two_bash_sessions_keep_their_own_stacks() {
    general::cross_sessions(&Lab::new("linux"), &BASH);
}

#[test]
fn a_changed_profile_asks_before_use() {
    general::drift(&Lab::new("linux"));
}

#[test]
fn unuse_names_what_it_pops() {
    general::unuse_needs_a_name_on_the_stack(&Lab::new("linux"));
}

#[test]
fn a_stale_stack_is_ignored() {
    general::a_stale_stack_is_ignored(&Lab::new("linux"));
}

#[test]
fn settings_come_from_the_config_file() {
    general::config_file(&Lab::new("linux"));
}

#[test]
fn create_writes_a_profile_and_refuses_to_clobber_it() {
    let lab = Bash::new();
    let mut c = Checks::default();
    c.is_true("create succeeds", lab.run(&["create", "work"]).status.success());
    c.is_true("create writes the .env", lab.profile_path("work").is_file());
    c.is_true("create does not clobber an existing profile", !lab.run(&["create", "work"]).status.success());
    c.is_true("create --force overwrites", lab.run(&["create", "work", "--force"]).status.success());
    c.done();
}

#[test]
fn list_shows_every_profile_and_the_startup_state() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.profile("broken", "EDITOR=ed\nTHIS LINE HAS NO EQUALS SIGN\n");

    let out = lab.run(&["list"]);
    let listing = stdout_of(&out);
    let mut c = Checks::default();
    c.has("list shows work", "work", &listing);
    c.has("list shows alt", "alt", &listing);
    c.has("list reports startup loading", "startup loading", &listing);
    c.has("list reports the startup profile", "startup profile", &listing);
    c.has("an unparseable profile is named on stderr", "broken", &stderr_of(&out));
    c.is_true("list itself succeeds", out.status.success());
    c.done();
}

#[test]
fn use_overrides_existing_values() {
    let lab = Bash::new();
    lab.two_profiles();
    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT TOKEN EXPLICIT
eval "$(envc use work)"
printf "%s|%s|%s|%s" "$EDITOR" "$PROJECT" "$TOKEN" "$EXPLICIT""#,
    );
    Checks::default().eq("use overrides existing values", "vim|work|a b|yes", &out).done();
}

#[test]
fn use_expands_path_from_the_environment() {
    let lab = Bash::new();
    lab.two_profiles();
    let out = lab.sh(
        r#"ENVC=$(command -v envc)
PATH=/usr/bin:/bin
eval "$($ENVC use work)"
printf "%s" "$PATH""#,
    );
    let expected = format!("{}/bin:/usr/bin:/bin", lab.home().display());
    Checks::default().eq("use expands $PATH from the environment", &expected, &out).done();
}

#[test]
fn unuse_restores_replaced_values() {
    let lab = Bash::new();
    lab.two_profiles();
    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT TOKEN
eval "$(envc use work)"
eval "$(envc unuse work)"
printf "%s|%s|%s" "$EDITOR" "${PROJECT-<unset>}" "${TOKEN-<unset>}""#,
    );
    Checks::default().eq("unuse puts the replaced values back", "nano|<unset>|<unset>", &out).done();
}

#[test]
fn unuse_restores_path_exactly() {
    let lab = Bash::new();
    lab.two_profiles();
    let out = lab.sh(
        r#"ENVC=$(command -v envc)
PATH=/usr/bin
eval "$($ENVC use work)"
eval "$($ENVC unuse work)"
printf "%s" "$PATH""#,
    );
    Checks::default().eq("unuse restores PATH exactly", "/usr/bin", &out).done();
}

#[test]
fn profiles_stack_and_pop_in_order() {
    let lab = Bash::new();
    lab.two_profiles();
    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT
eval "$(envc push work)"
printf "work=%s,%s " "$EDITOR" "$PROJECT"
eval "$(envc push alt)"
printf "alt=%s,%s,%s " "$EDITOR" "$PROJECT" "$ENVC_ACTIVE"
eval "$(envc pop alt)"
printf "back=%s,%s,%s " "$EDITOR" "$PROJECT" "$ENVC_ACTIVE"
eval "$(envc pop work)"
printf "end=%s,%s" "$EDITOR" "${PROJECT-<unset>}""#,
    );
    Checks::default()
        .eq(
            "work -> alt -> pop alt -> pop work",
            "work=vim,work alt=emacs,alt,work:alt back=vim,work,work end=nano,<unset>",
            &out,
        )
        .done();
}

#[test]
fn the_restore_stack_is_a_diff_shaped_file() {
    let lab = Bash::new();
    lab.two_profiles();
    let shown = lab.sh(r#"EDITOR=nano; export EDITOR; unset PROJECT; eval "$(envc use work)"; envc stack"#);
    let stack = lab.stacks().concat();

    let mut c = Checks::default();
    c.has("the stack has the '-' side", "-EDITOR=nano", &stack);
    c.has("the stack has the '+' side", "+EDITOR=vim", &stack);
    c.has("the stack records variables that did not exist", "-PROJECT", &stack);
    c.has("the stack names the profile", "@@ work ", &stack);
    c.has("envc stack prints the file", "-EDITOR=nano", &shown);
    c.done();
}

#[test]
fn a_profile_is_parsed_the_way_the_readme_says() {
    let lab = Bash::new();
    lab.profile(
        "parser",
        "# comment\n\
         KEY=value\n\
         export EXPORTED=yes\n\
         QUOTED=\"hello world\"\n\
         LITERAL='$HOME not expanded'\n\
         TRAILING=abc # trailing comment\n\
         NOT_A_COMMENT=abc#def\n\
         ROOT=/opt/app\n\
         BIN=$ROOT/bin\n\
         FALLBACK=${UNSET_VAR:-fallback}\n\
         FROM_ENV=x$FROM_ENV\n",
    );
    let out = lab.sh(
        r#"FROM_ENV=base; export FROM_ENV
unset UNSET_VAR
eval "$(envc use parser)"
printf "%s|%s|%s|%s|%s|%s|%s|%s|%s" \
    "$KEY" "$EXPORTED" "$QUOTED" "$LITERAL" "$TRAILING" "$NOT_A_COMMENT" "$BIN" "$FALLBACK" "$FROM_ENV""#,
    );
    Checks::default()
        .eq(
            "plain, double quotes, single quotes, comments, same-file expansion, defaults, environment expansion",
            "value|yes|hello world|$HOME not expanded|abc|abc#def|/opt/app/bin|fallback|xbase",
            &out,
        )
        .done();
}

#[test]
fn a_broken_profile_is_reported_not_half_applied() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.profile("broken", "EDITOR=ed\nTHIS LINE HAS NO EQUALS SIGN\n");

    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR; unset PROJECT
eval "$(envc use work)"
eval "$(envc use broken 2>/dev/null)"
printf "%s|%s" "$EDITOR" "$ENVC_ACTIVE""#,
    );
    let failed = lab.run(&["use", "broken"]);

    let mut c = Checks::default();
    c.eq("a failed use prints no shell code and leaves the stack alone", "vim|work", &out);
    c.is_true("a broken profile fails", !failed.status.success());
    c.has("the error names the file and line", "broken/.env:2", &stderr_of(&failed));
    c.lacks("a failed use does not reach the stack", "@@ broken", &lab.stacks().concat());
    c.done();
}

#[test]
fn init_installs_the_hook_and_is_idempotent() {
    let lab = Bash::new();
    let mut c = Checks::default();

    c.is_true("init succeeds", lab.run(&["init"]).status.success());
    let rc = fs::read_to_string(lab.rc()).unwrap();
    c.has("the hook is in the rc file", "# >>> envc initialize >>>", &rc);
    c.has("the hook calls autoload", "envc autoload", &rc);
    c.has("the hook wraps use/unuse", "use|push|unuse|pop", &rc);

    lab.run(&["init"]);
    let again = fs::read_to_string(lab.rc()).unwrap();
    c.eq("init can be re-run", "1", &again.matches("# >>> envc initialize >>>").count().to_string());
    c.is_true("the rc file keeps its own content", again.starts_with("export KEEPME=1"));
    c.done();
}

#[test]
fn the_wrapper_evaluates_use_and_unuse() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["init"]);
    let out = lab.sourced(
        r#"unset PROJECT
envc use alt 2>/dev/null
printf "%s " "$PROJECT"
envc unuse alt 2>/dev/null
printf "%s" "${PROJECT-<unset>}""#,
    );
    Checks::default().eq("with the hook, use/unuse work directly", "alt <unset>", &out).done();
}

#[test]
fn enable_selects_what_new_shells_load() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["init"]);
    let mut c = Checks::default();

    c.is_true("enable without a name fails", !lab.run(&["enable"]).status.success());
    c.is_true("enable of a missing profile fails", !lab.run(&["enable", "ghost"]).status.success());
    c.is_true("enable work succeeds", lab.run(&["enable", "work"]).status.success());
    c.has("enable records the choice", "work", &lab.read("startup"));

    let out = lab.sourced(r#"printf "%s|%s" "$PROJECT" "$ENVC_ACTIVE""#);
    c.eq("a new shell autoloads the startup profile", "work|work", &out);
    c.done();
}

#[test]
fn enable_does_not_touch_the_current_shell() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["init"]);
    let out = lab.sh(r#"envc enable work >/dev/null 2>&1; printf "%s|%s" "${PROJECT-<unset>}" "${ENVC_ACTIVE-<none>}""#);
    Checks::default().eq("enable leaves the current shell alone", "<unset>|<none>", &out).done();
}

#[test]
fn disable_names_the_startup_profile() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["enable", "work"]);
    let mut c = Checks::default();

    c.is_true("disable without a name fails", !lab.run(&["disable"]).status.success());
    let wrong = lab.run(&["disable", "alt"]);
    c.is_true("disable of a profile that is not the startup one fails", !wrong.status.success());
    c.has("and says which one is", "'work' is", &stderr_of(&wrong));
    c.has("the choice is untouched", "work", &lab.read("startup"));
    c.is_true("disable work succeeds", lab.run(&["disable", "work"]).status.success());
    c.done();
}

#[test]
fn disable_stops_loading_and_forgets_the_choice() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);
    lab.run(&["disable", "work"]);

    let mut c = Checks::default();
    c.eq("disable clears the choice", "", &lab.read("startup"));
    let out = lab.sourced(r#"printf "%s|%s" "${PROJECT-<unset>}" "${ENVC_ACTIVE-<none>}""#);
    c.eq("new shells load nothing afterwards", "<unset>|<none>", &out);
    c.is_true("a second disable fails: nothing is enabled", !lab.run(&["disable", "work"]).status.success());
    c.is_true("enable brings it back", lab.run(&["enable", "work"]).status.success());
    c.done();
}

#[test]
fn disable_restores_the_rc_file_byte_for_byte() {
    let lab = Bash::new();
    lab.two_profiles();
    let original = fs::read_to_string(lab.rc()).unwrap();
    lab.run(&["enable", "work"]);
    lab.run(&["disable", "work"]);
    Checks::default()
        .eq("after disable the rc file is byte-for-byte what it was", &original, &fs::read_to_string(lab.rc()).unwrap())
        .done();
}

#[test]
fn the_two_halves_are_independent() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);
    let mut c = Checks::default();

    let after = lab.sourced(r#"envc unuse work >/dev/null 2>&1; printf "%s|%s" "${PROJECT-<unset>}" "${ENVC_ACTIVE-<none>}""#);
    c.eq("unuse only clears the current shell", "<unset>|<none>", &after);
    c.eq("unuse does not stop new shells from autoloading", "work", &lab.sourced(r#"printf "%s" "$PROJECT""#));

    let mixed = lab.sourced(
        r#"envc use alt >/dev/null 2>&1
printf "%s|%s" "$PROJECT" "$(cat "$HOME/.envc/startup")""#,
    );
    c.eq("use does not change the startup choice", "alt|work", &mixed);
    c.eq("new shells ignore a temporary use", "work", &lab.sourced(r#"printf "%s" "$PROJECT""#));
    c.done();
}

#[test]
fn an_install_from_before_the_startup_file_keeps_working() {
    let lab = Bash::new();
    lab.profile("work", "PROJECT=work\n");
    fs::write(
        lab.envc_home().join("stack"),
        "# envc restore stack\n# format 1\n# active-profile work\n--- before envc\n+++ after `work`\n-PROJECT\n+PROJECT=work\n",
    )
    .unwrap();

    let mut c = Checks::default();
    let out = lab.run(&["autoload"]);
    c.is_true("autoload adopts the old choice", out.status.success());
    c.has("and says so", "startup profile", &stderr_of(&out));
    c.has("the adoption is persisted", "work", &lab.read("startup"));
    c.has("and applied", "export PROJECT=work", &stdout_of(&out));
    c.is_true("the old single-file stack became a directory", lab.envc_home().join("stack").is_dir());
    c.is_true("use works afterwards", lab.run(&["use", "work"]).status.success());
    c.done();
}

#[test]
fn delete_refuses_profiles_in_use_and_at_startup() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["enable", "work"]);
    let mut c = Checks::default();

    let in_use = lab.sh(r#"eval "$(envc use alt 2>/dev/null)"; envc delete alt 2>&1"#);
    c.has("a profile in use cannot be deleted", "in use; run `envc unuse alt` first", &in_use);

    let startup = lab.run(&["delete", "work"]);
    c.is_true("the startup profile cannot be deleted either", !startup.status.success());
    c.has("and says it is the startup profile", "startup profile", &stderr_of(&startup));
    c.has("and points at disable", "envc disable", &stderr_of(&startup));

    let forced = lab.sh(r#"eval "$(envc use alt 2>/dev/null)"; envc delete alt --force 2>&1; eval "$(envc unuse alt 2>/dev/null)"; printf "[%s]" "${PROJECT-<unset>}""#);
    c.has("--force deletes it and warns that it is still on the stack", "still on the restore stack", &forced);
    c.has("unuse still works after the delete", "[<unset>]", &forced);
    c.is_true("deleting a missing profile fails", !lab.run(&["delete", "nonexistent"]).status.success());
    c.done();
}

#[test]
fn bad_profile_names_are_rejected() {
    let lab = Bash::new();
    let mut c = Checks::default();
    for bad in ["../evil", "a/b", ".hidden", ""] {
        c.is_true(&format!("create rejects the name {bad:?}"), !lab.run(&["create", bad]).status.success());
    }
    c.is_true("use of a missing profile fails", !lab.run(&["use", "nonexistent"]).status.success());
    c.is_true("status succeeds", lab.run(&["status"]).status.success());
    c.done();
}

#[test]
fn powershell_gets_powershell_syntax() {
    let lab = Bash::new();
    lab.profile("ps", "FOO=bar baz\nQUOTED=$SRC\nPCT=50%\n");
    let out = lab
        .command(ENVC)
        .args(["--shell", "powershell", "use", "ps"])
        .env("SRC", "it's")
        .output()
        .unwrap();
    let code = stdout_of(&out);

    let mut c = Checks::default();
    c.has("a value with spaces is quoted", "$env:FOO='bar baz'", &code);
    c.has("inner single quotes are doubled", "$env:QUOTED='it''s'", &code);
    c.has("% needs no escaping in PowerShell", "$env:PCT='50%'", &code);
    c.has("the current stack is recorded", "$env:ENVC_ACTIVE='ps'", &code);
    c.done();
}

#[test]
fn cmd_gets_a_batch_file_instead_of_stdout() {
    let lab = Bash::new();
    lab.profile("c", "FOO=bar baz\nPCT=50%\n");
    let out = lab.run(&["--shell", "cmd", "use", "c"]);
    let script = fs::read_to_string(lab.batch("use")).unwrap_or_default();

    let mut c = Checks::default();
    c.eq("cmd gets nothing on stdout", "", &stdout_of(&out));
    c.has("and says which file to call", "call", &stderr_of(&out));
    c.has("the batch file starts with echo off", "@echo off", &script);
    c.has("assignments use set", "set \"FOO=bar baz\"", &script);
    c.has("% is escaped as %%", "set \"PCT=50%%\"", &script);
    c.has("the current stack is recorded", "set \"ENVC_ACTIVE=c\"", &script);
    c.has("lines end in CRLF", "\r\n", &script);
    c.done();
}

#[test]
fn cmd_unuse_writes_the_undo_script() {
    let lab = Bash::new();
    lab.two_profiles();
    lab.run(&["--shell", "cmd", "use", "work"]);
    lab.run_as("work", &["--shell", "cmd", "unuse", "work"]);
    let script = fs::read_to_string(lab.batch("unuse")).unwrap_or_default();

    let mut c = Checks::default();
    c.has("a variable that did not exist is cleared", "set \"EDITOR=\"", &script);
    c.has("a variable that existed is restored", "set \"PATH=", &script);
    c.has("ENVC_ACTIVE is cleared", "set \"ENVC_ACTIVE=\"", &script);
    c.done();
}

#[test]
fn an_unknown_shell_is_rejected() {
    let lab = Bash::new();
    let out = lab.run(&["--shell", "tcsh", "use", "work"]);
    let mut c = Checks::default();
    c.is_true("an unknown shell fails", !out.status.success());
    c.has("and lists the choices", "expected bash, powershell or cmd", &stderr_of(&out));
    c.done();
}

#[test]
fn quotes_inside_a_value_survive() {
    let lab = Bash::new();
    lab.profile(
        "q",
        "A=it's\nB=don't stop\nC=\"double says 'hi'\"\nD='literal'\nE=a\"b\nF=x'y'z\nG=\"a\"'b'\n",
    );
    let out = lab.sh(
        r#"unset A B C D E F G
eval "$(envc use q)"
printf "%s|%s|%s|%s|%s|%s|%s" "$A" "$B" "$C" "$D" "$E" "$F" "$G""#,
    );
    Checks::default()
        .eq("quotes inside a value are kept", "it's|don't stop|double says 'hi'|literal|a\"b|x'y'z|ab", &out)
        .done();
}

#[test]
fn a_quoted_value_can_span_lines() {
    let lab = Bash::new();
    lab.profile("q", "MULTI=\"first\nsecond\nthird\"\nSINGLE='raw\n$HOME stays'\nAFTER=ok\n");
    let out = lab.sh(
        r#"unset MULTI SINGLE AFTER
eval "$(envc use q)"
printf "%s|%s|%s" "$MULTI" "$SINGLE" "$AFTER""#,
    );
    Checks::default().eq("a quoted value can span lines", "first\nsecond\nthird|raw\n$HOME stays|ok", &out).done();
}

#[test]
fn an_unterminated_quote_is_reported_with_its_line() {
    let lab = Bash::new();
    lab.profile("q", "A=1\nB=\"oops\nC=2\n");
    let out = lab.run(&["use", "q"]);
    let mut c = Checks::default();
    c.is_true("an unterminated quote fails", !out.status.success());
    c.has("it names the line the quote opened on", "q/.env:2", &stderr_of(&out));
    c.has("and says which quote", "unterminated \" quote", &stderr_of(&out));
    c.done();
}

#[test]
fn the_legacy_wrapper_is_gone() {
    let lab = Bash::new();
    let mut c = Checks::default();
    for old in ["activate", "deactivate", "de"] {
        c.is_true(&format!("{old} is gone"), !lab.run(&[old, "x"]).status.success());
    }
    c.done();
}
