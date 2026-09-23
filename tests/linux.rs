//! linux: envc 在 Linux / bash 上的集成测试，跑 `cargo test` 即执行。
//!
//! 三个平台各一个文件，靠下面的 `#![cfg]` 条件编译挑选：非 Linux 平台上这个
//! crate 会被整个清空，一条测试都不会编译进来。
#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const ENVC: &str = env!("CARGO_BIN_EXE_envc");

// ===========================================================================
// 沙箱
// ===========================================================================

/// 一次性的家目录 + rc 文件 + PATH。
///
/// 所有会改环境的行为都指向这里，测试碰不到真实的家目录，多个测试也能并行跑。
struct Lab {
    root: PathBuf,
}

impl Lab {
    fn new() -> Lab {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "envc-linux-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        // 子 shell 里要能直接敲 `envc`，所以放一个软链进 PATH。
        symlink(ENVC, root.join("bin/envc")).unwrap();

        let lab = Lab { root };
        fs::create_dir_all(lab.home()).unwrap();
        fs::write(lab.rc(), "export KEEPME=1\n").unwrap();
        lab
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn envc_home(&self) -> PathBuf {
        self.home().join(".envc")
    }

    fn rc(&self) -> PathBuf {
        self.home().join(".bashrc")
    }

    fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut path = vec![self.root.join("bin")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        vec![
            ("HOME", self.home().into()),
            ("ENVC_HOME", self.envc_home().into()),
            ("ENVC_RC", self.rc().into()),
            ("SHELL", "/bin/bash".into()),
            ("PATH", std::env::join_paths(path).unwrap()),
            // cmd 的批处理文件落在临时目录里；指向沙箱，免得多测试互踩。
            ("TMPDIR", self.root.join("tmp").into()),
        ]
    }

    /// cmd 用的那个批处理文件。`std::env::temp_dir()` 认 TMPDIR，所以它落在
    /// 这个沙箱里。
    fn batch(&self, what: &str) -> PathBuf {
        self.root.join("tmp").join("envc").join(format!("{what}.cmd"))
    }

    /// 直接跑二进制，不经 PATH。
    fn run(&self, args: &[&str]) -> Output {
        Command::new(ENVC)
            .args(args)
            // env_clear 之后只喂我们要的那几个：调用方的 EDITOR 之类漏进来，
            // 就会变成「activate 之前的值」而影响还原断言。
            .env_clear()
            .envs(self.env())
            // 调用方环境里若漏进来一个，就会被当成「这个 shell 装了什么」。
            // 置空字符串不算 unset，所以是删掉。
            .env_remove("ENVC_ACTIVE")
            .output()
            .expect("envc 应该能启动")
    }

    /// 在一个真的 bash 里跑脚本，返回它的 stdout（去掉行尾换行）。
    fn sh(&self, script: &str) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env_clear()
            .envs(self.env())
            .env_remove("ENVC_ACTIVE")
            .output()
            .expect("bash 应该能启动");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    /// `source "<rc>"; ` —— 用来拼脚本。
    ///
    /// 拼的时候用 `push_str` 而不是 `format!`：脚本片段里全是 `${...}`，
    /// 交给 `format!` 会被当成占位符。
    fn source_rc(&self) -> String {
        format!("source \"{}\"; ", self.rc().display())
    }

    /// 一个 source 过 rc 的 shell 里跑的脚本。
    fn sourced(&self, body: &str) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!("{}{body}", self.source_rc()))
            .env_clear()
            .envs(self.env())
            .env_remove("ENVC_ACTIVE")
            .output()
            .expect("bash 应该能启动");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn profile(&self, name: &str, body: &str) {
        let dir = self.envc_home().join("profiles").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".env"), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.envc_home().join(rel)).unwrap_or_default()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `work` 与 `alt` 两个 profile，覆盖「已存在」「不存在」「带空格」几种取值。
fn two_profiles(lab: &Lab) {
    lab.profile(
        "work",
        "# the profile used by most tests\n\
         EDITOR=vim\n\
         PROJECT=work\n\
         TOKEN=\"a b\"\n\
         PATH=\"$HOME/bin:$PATH\"\n\
         export EXPLICIT=yes\n",
    );
    lab.profile("alt", "EDITOR=emacs\nPROJECT=alt\n");
}

// ===========================================================================
// 断言
// ===========================================================================

/// 攒着失败而不是第一条就 panic，一次能看到这一组里所有问题。
#[derive(Default)]
struct Checks {
    failed: Vec<String>,
}

impl Checks {
    fn eq(&mut self, what: &str, expected: &str, actual: &str) -> &mut Self {
        if expected != actual {
            self.failed.push(format!(
                "{what}\n       expected: {expected}\n       actual:   {actual}"
            ));
        }
        self
    }

    fn has(&mut self, what: &str, needle: &str, hay: &str) -> &mut Self {
        if !hay.contains(needle) {
            self.failed.push(format!("{what}\n       {needle} 不在:\n{hay}"));
        }
        self
    }

    fn is_true(&mut self, what: &str, yes: bool) -> &mut Self {
        if !yes {
            self.failed.push(what.to_string());
        }
        self
    }

    fn done(&mut self) {
        assert!(
            self.failed.is_empty(),
            "\n{} 处失败:\n{}",
            self.failed.len(),
            self.failed.join("\n")
        );
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim_end().to_string()
}

// ===========================================================================
// profile 的增删查
// ===========================================================================

#[test]
fn create_writes_a_profile_and_refuses_to_clobber_it() {
    let lab = Lab::new();
    let mut c = Checks::default();

    c.is_true("create 应该成功", lab.run(&["create", "work"]).status.success());
    c.is_true(
        "create 应该写出 .env",
        lab.envc_home().join("profiles/work/.env").is_file(),
    );
    c.is_true(
        "create 不该覆盖已有的 profile",
        !lab.run(&["create", "work"]).status.success(),
    );
    c.is_true(
        "create --force 才覆盖",
        lab.run(&["create", "work", "--force"]).status.success(),
    );

    c.done();
}

#[test]
fn list_shows_every_profile_and_the_startup_state() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.profile("broken", "EDITOR=ed\nTHIS LINE HAS NO EQUALS SIGN\n");

    let out = lab.run(&["list"]);
    let listing = stdout_of(&out);
    let mut c = Checks::default();

    c.has("list 列出 work", "work", &listing);
    c.has("list 列出 alt", "alt", &listing);
    c.has("list 报告启动状态", "startup loading", &listing);
    c.has("list 报告启动 profile", "startup profile", &listing);
    c.has("无法解析的 profile 会在 stderr 里被点名", "broken", &stderr_of(&out));
    c.is_true("list 本身应该成功", out.status.success());

    c.done();
}

// ===========================================================================
// activate / deactivate：当前 shell
// ===========================================================================

#[test]
fn activate_overrides_existing_values() {
    let lab = Lab::new();
    two_profiles(&lab);

    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT TOKEN EXPLICIT
eval "$(envc activate work)"
printf "%s|%s|%s|%s" "$EDITOR" "$PROJECT" "$TOKEN" "$EXPLICIT""#,
    );

    let mut c = Checks::default();
    c.eq("activate 覆盖已有值", "vim|work|a b|yes", &out);
    c.done();
}

#[test]
fn activate_expands_path_from_the_environment() {
    let lab = Lab::new();
    two_profiles(&lab);

    // 先记住 envc 的位置，再换掉 PATH，否则待会儿找不到它。
    let out = lab.sh(
        r#"ENVC=$(command -v envc)
PATH=/usr/bin:/bin
eval "$($ENVC activate work)"
printf "%s" "$PATH""#,
    );

    let mut c = Checks::default();
    let expected = format!("{}/bin:/usr/bin:/bin", lab.home().display());
    c.eq("activate 会展开环境里的 $PATH", &expected, &out);
    c.done();
}

#[test]
fn deactivate_restores_replaced_values() {
    let lab = Lab::new();
    two_profiles(&lab);

    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT TOKEN
eval "$(envc activate work)"
eval "$(envc deactivate)"
printf "%s|%s|%s" "$EDITOR" "${PROJECT-<unset>}" "${TOKEN-<unset>}""#,
    );

    let mut c = Checks::default();
    c.eq("deactivate 把被覆盖的值放回去", "nano|<unset>|<unset>", &out);
    c.done();
}

#[test]
fn deactivate_restores_path_exactly() {
    let lab = Lab::new();
    two_profiles(&lab);

    let out = lab.sh(
        r#"ENVC=$(command -v envc)
PATH=/usr/bin
eval "$($ENVC activate work)"
eval "$($ENVC deactivate)"
printf "%s" "$PATH""#,
    );

    let mut c = Checks::default();
    c.eq("deactivate 精确还原 PATH", "/usr/bin", &out);
    c.done();
}

#[test]
fn switching_profiles_rolls_back_to_the_original_base() {
    let lab = Lab::new();
    two_profiles(&lab);

    let out = lab.sh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT
eval "$(envc activate work)"
printf "mid=%s,%s " "$EDITOR" "${PROJECT-<unset>}"
eval "$(envc activate alt)"
printf "switched=%s,%s " "$EDITOR" "${PROJECT-<unset>}"
eval "$(envc deactivate)"
printf "end=%s,%s" "$EDITOR" "${PROJECT-<unset>}""#,
    );

    let mut c = Checks::default();
    c.eq(
        "work -> alt -> deactivate 回到最初的 shell",
        "mid=vim,work switched=emacs,alt end=nano,<unset>",
        &out,
    );
    c.done();
}

// ===========================================================================
// 恢复栈
// ===========================================================================

#[test]
fn the_restore_stack_is_a_diff_shaped_file() {
    let lab = Lab::new();
    two_profiles(&lab);

    lab.sh(r#"EDITOR=nano; export EDITOR; unset PROJECT; eval "$(envc activate work)""#);
    let stack = lab.read("stack");

    let mut c = Checks::default();
    c.has("栈里有 '-' 一侧", "-EDITOR=nano", &stack);
    c.has("栈里有 '+' 一侧", "+EDITOR=vim", &stack);
    c.has("栈记下了原本不存在的变量", "-PROJECT", &stack);
    c.has("栈写明了 profile", "# active-profile work", &stack);
    c.has("栈带 diff 头", "--- before envc", &stack);
    c.has("envc stack 会把文件打出来", "-EDITOR=nano", &stdout_of(&lab.run(&["stack"])));

    c.done();
}

// ===========================================================================
// 解析 .env：引号、展开、默认值、注释
// ===========================================================================

#[test]
fn a_profile_is_parsed_the_way_the_readme_says() {
    let lab = Lab::new();
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

    // 在真 shell 里求值，比断言生成的字符串更可靠。
    let out = lab.sh(
        r#"FROM_ENV=base; export FROM_ENV
unset UNSET_VAR
eval "$(envc activate parser)"
printf "%s|%s|%s|%s|%s|%s|%s|%s|%s" \
    "$KEY" "$EXPORTED" "$QUOTED" "$LITERAL" "$TRAILING" "$NOT_A_COMMENT" "$BIN" "$FALLBACK" "$FROM_ENV""#,
    );

    let mut c = Checks::default();
    c.eq(
        "普通赋值、双引号、单引号、注释、同文件展开、默认值、环境展开",
        "value|yes|hello world|$HOME not expanded|abc|abc#def|/opt/app/bin|fallback|xbase",
        &out,
    );
    c.done();
}

#[test]
fn a_broken_profile_is_reported_not_half_applied() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.profile("broken", "EDITOR=ed\nTHIS LINE HAS NO EQUALS SIGN\n");

    // 先让它有一个干净的栈，好验证失败时栈没被动过。
    lab.sh(r#"EDITOR=nano; export EDITOR; unset PROJECT; eval "$(envc activate work)""#);

    let failed = lab.run(&["activate", "broken"]);
    let mut c = Checks::default();
    c.is_true("坏 profile 应该报错退出", !failed.status.success());
    c.has("报错要指出文件与行号", "broken/.env:2", &stderr_of(&failed));
    c.has(
        "失败的 activate 不该动栈",
        "# active-profile work",
        &lab.read("stack"),
    );

    let unchanged = lab.sh(r#"EDITOR=nano; eval "$(envc activate broken 2>/dev/null)"; printf "%s" "$EDITOR""#);
    c.eq("失败的 activate 不该吐出任何 shell 代码", "nano", &unchanged);

    c.done();
}

// ===========================================================================
// init：装钩子
// ===========================================================================

#[test]
fn init_installs_the_hook_and_is_idempotent() {
    let lab = Lab::new();
    let mut c = Checks::default();

    c.is_true("init 应该成功", lab.run(&["init"]).status.success());
    let rc = fs::read_to_string(lab.rc()).unwrap();
    c.has("钩子进了 rc 文件", "# >>> envc initialize >>>", &rc);
    c.has("钩子调用 autoload", "envc autoload", &rc);

    lab.run(&["init"]);
    let again = fs::read_to_string(lab.rc()).unwrap();
    c.eq("init 可以重复执行", "1", &again.matches("# >>> envc initialize >>>").count().to_string());
    c.is_true("rc 里原有的内容还在", again.starts_with("export KEEPME=1"));

    c.done();
}

// ===========================================================================
// enable / disable：启动加载
// ===========================================================================

#[test]
fn enable_selects_what_new_shells_load() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["init"]);

    let mut c = Checks::default();

    // 干净的家目录里没有可沿用的选择，裸跑应该拒绝。
    let err = lab.run(&["enable"]);
    c.is_true("没有可沿用的选择时，裸 enable 应该失败", !err.status.success());
    c.has("并且告诉用户怎么选", "envc enable <name>", &stderr_of(&err));

    c.is_true("enable 不存在的 profile 应该失败", !lab.run(&["enable", "ghost"]).status.success());
    c.is_true("enable work 应该成功", lab.run(&["enable", "work"]).status.success());
    c.has("enable 把选择落盘", "work", &lab.read("startup"));

    // 新 shell（source 了 rc）应该加载 work。
    let out = lab.sourced(r#"printf "%s|%s" "$PROJECT" "$ENVC_ACTIVE""#);
    c.eq("新 shell 自动加载启动 profile", "work|work", &out);

    c.done();
}

#[test]
fn enable_does_not_touch_the_current_shell() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["init"]);

    // enable 只安排「下一个 shell」，当前 shell 不该有 PROJECT。
    let out = lab.sh(r#"envc enable work >/dev/null 2>&1; printf "%s|%s" "${PROJECT-<unset>}" "${ENVC_ACTIVE-<none>}""#);

    let mut c = Checks::default();
    c.eq("enable 不动当前 shell", "<unset>|<none>", &out);
    c.done();
}

#[test]
fn bare_enable_restores_the_remembered_profile() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["init"]);

    lab.run(&["enable", "work"]);
    lab.run(&["disable"]);

    let mut c = Checks::default();
    c.is_true("disable 之后裸 enable 应该成功", lab.run(&["enable"]).status.success());
    c.has("沿用的还是原来那个", "work", &lab.read("startup"));
    c.done();
}

#[test]
fn disable_stops_loading_but_keeps_the_choice() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);

    lab.run(&["disable"]);

    let mut c = Checks::default();
    c.has("disable 仍然记着选择", "work", &lab.read("startup"));

    let out = lab.sourced(r#"printf "%s|%s" "${PROJECT-<unset>}" "${ENVC_ACTIVE-<none>}""#);
    c.eq("关掉之后新 shell 不加载任何东西", "<unset>|<none>", &out);

    c.is_true("disable 可以重复执行", lab.run(&["disable"]).status.success());
    c.done();
}

#[test]
fn disable_restores_the_rc_file_byte_for_byte() {
    let lab = Lab::new();
    let original = fs::read_to_string(lab.rc()).unwrap();

    lab.run(&["enable", "work"]);
    lab.run(&["disable"]);

    let mut c = Checks::default();
    c.eq("disable 之后 rc 文件与装钩子前逐字节相同", &original, &fs::read_to_string(lab.rc()).unwrap());
    c.done();
}

#[test]
fn the_two_halves_are_independent() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);

    let mut c = Checks::default();

    // deactivate 只清它所在的那个 shell，不该影响新 shell。
    let after = lab.sourced(r#"envc deactivate >/dev/null 2>&1; printf "%s" "$(envc --version)""#);
    c.has("deactivate 之后 envc 依然可用", "envc", &after);

    let fresh = lab.sourced(r#"printf "%s" "$PROJECT""#);
    c.eq("deactivate 不阻止新 shell 自动加载", "work", &fresh);

    // activate 是另一半：它不该改启动选择。
    let mixed = lab.sourced(
        r#"envc activate alt >/dev/null 2>&1
printf "%s|%s" "$PROJECT" "$(sed -n 's/^\([^#].*\)$/\1/p' "$ENVC_HOME/startup" | head -1)""#,
    );
    c.eq("activate 不动启动选择", "alt|work", &mixed);

    let untouched = lab.sourced(r#"printf "%s" "$PROJECT""#);
    c.eq("新 shell 不理会临时的 activate", "work", &untouched);

    c.done();
}

#[test]
fn an_install_from_before_the_startup_file_keeps_working() {
    let lab = Lab::new();

    // 造一个「旧版安装」：有还原栈（那时 activate 就是选择的来源），没有 startup。
    let legacy_home = lab.root.join("legacy/.envc");
    fs::create_dir_all(legacy_home.join("profiles/work")).unwrap();
    fs::write(legacy_home.join("profiles/work/.env"), "PROJECT=work\n").unwrap();

    let legacy_rc = lab.root.join("legacy/.bashrc");
    fs::write(&legacy_rc, "").unwrap();

    let legacy = |args: &[&str]| {
        Command::new(ENVC)
            .args(args)
            .env("HOME", lab.root.join("legacy"))
            .env("ENVC_HOME", &legacy_home)
            .env("ENVC_RC", &legacy_rc)
            .env("SHELL", "/bin/bash")
            .output()
            .unwrap()
    };

    legacy(&["activate", "work"]);

    let mut c = Checks::default();
    c.is_true(
        "旧版安装没有 startup 文件",
        !legacy_home.join("startup").exists(),
    );

    let out = legacy(&["enable"]);
    c.is_true("裸 enable 应该沿用旧的选择", out.status.success());
    c.has("并且说明发生了迁移", "startup profile", &stderr_of(&out));
    c.has(
        "迁移结果落盘",
        "work",
        &fs::read_to_string(legacy_home.join("startup")).unwrap_or_default(),
    );

    c.done();
}

// ===========================================================================
// delete
// ===========================================================================

#[test]
fn delete_refuses_active_and_startup_profiles() {
    let lab = Lab::new();
    two_profiles(&lab);
    lab.run(&["enable", "work"]);
    lab.run(&["activate", "alt"]);

    let mut c = Checks::default();

    let active = lab.run(&["delete", "alt"]);
    c.is_true("正在用的 profile 不能删", !active.status.success());
    c.has("并说明原因", "currently active", &stderr_of(&active));

    let startup = lab.run(&["delete", "work"]);
    c.is_true("启动 profile 也不能删", !startup.status.success());
    c.has("并点名它是启动 profile", "startup profile", &stderr_of(&startup));
    c.has("并指向 disable", "envc disable", &stderr_of(&startup));

    c.is_true("--force 可以删", lab.run(&["delete", "alt", "--force"]).status.success());
    c.is_true(
        "--force 会清掉还原栈",
        !lab.envc_home().join("stack").exists(),
    );
    c.is_true("删不存在的 profile 会报错", !lab.run(&["delete", "nonexistent"]).status.success());

    c.done();
}

// ===========================================================================
// 坏输入
// ===========================================================================

#[test]
fn bad_profile_names_are_rejected() {
    let lab = Lab::new();
    let mut c = Checks::default();

    for bad in ["../evil", "a/b", ".hidden", ""] {
        c.is_true(
            &format!("create 应该拒绝名字 {bad:?}"),
            !lab.run(&["create", bad]).status.success(),
        );
    }

    c.is_true("activate 不存在的 profile 会报错", !lab.run(&["activate", "nonexistent"]).status.success());
    c.is_true("没有活动 profile 时 deactivate 不算错", lab.run(&["deactivate"]).status.success());
    c.is_true("status 应该成功", lab.run(&["status"]).status.success());

    c.done();
}

// ===========================================================================
// 别的 shell 的语法
// ===========================================================================
//
// cmd 和 PowerShell 只在 Windows 上能用，但**生成**它们的语法这一步是平台无关
// 的，所以在这里就能测——`--shell` 存在的意义正在于此，Windows 套件里只需要
// 再测那些真正依赖系统的东西。

#[test]
fn powershell_gets_powershell_syntax() {
    let lab = Lab::new();
    // 值里的单引号只能经环境展开进来：.env 里的单引号是引号语法本身，
    // 写在值中间会被解析器吃掉（见 tests 里那条 known-bug 断言的说明）。
    lab.profile("ps", "FOO=bar baz\nQUOTED=$SRC\nPCT=50%\n");

    let out = Command::new(ENVC)
        .args(["--shell", "powershell", "activate", "ps"])
        .env_clear()
        .envs(lab.env())
        .env("SRC", "it's")
        .env_remove("ENVC_ACTIVE")
        .output()
        .expect("envc 应该能启动");
    let code = stdout_of(&out);

    let mut c = Checks::default();
    c.has("空格的值要引起来", "$env:FOO='bar baz'", &code);
    // PowerShell 的转义是引号加倍，不是 bash 那种关掉再打开。
    c.has("内部单引号要加倍", "$env:QUOTED='it''s'", &code);
    c.has("% 在 PowerShell 里不用转义", "$env:PCT='50%'", &code);
    c.has("记录当前 profile", "$env:ENVC_ACTIVE='ps'", &code);
    c.done();
}

#[test]
fn cmd_gets_a_batch_file_instead_of_stdout() {
    let lab = Lab::new();
    lab.profile("c", "FOO=bar baz\nPCT=50%\n");

    let out = lab.run(&["--shell", "cmd", "activate", "c"]);

    let mut c = Checks::default();
    // cmd 没有 eval，所以 stdout 上不该有东西可求值。
    c.eq("cmd 的 stdout 是空的", "", &stdout_of(&out));
    c.has("并且告诉用户去 call 哪个文件", "call", &stderr_of(&out));

    let script = fs::read_to_string(lab.batch("activate")).unwrap_or_default();
    c.has("批处理文件以 echo off 开头", "@echo off", &script);
    c.has("用 set 赋值", "set \"FOO=bar baz\"", &script);
    // 批处理文件里 % 必须写两遍，否则会被当成变量引用。
    c.has("% 要转义成 %%", "set \"PCT=50%%\"", &script);
    c.has("记录当前 profile", "set \"ENVC_ACTIVE=c\"", &script);
    c.has("换行是 CRLF", "\r\n", &script);
    c.done();
}

#[test]
fn cmd_deactivate_writes_the_undo_script() {
    let lab = Lab::new();
    two_profiles(&lab);

    lab.run(&["--shell", "cmd", "activate", "work"]);
    lab.run(&["--shell", "cmd", "deactivate"]);

    let script = fs::read_to_string(lab.batch("deactivate")).unwrap_or_default();
    let mut c = Checks::default();
    // activate 之前 EDITOR 并不存在，所以撤销是置空而不是还原成旧值。
    c.has("原本不存在的变量要置空", "set \"EDITOR=\"", &script);
    // PATH 本来就有，所以它得被还原成原来的值。
    c.has("本来就有的变量要还原", "set \"PATH=", &script);
    c.has("清掉 ENVC_ACTIVE", "set \"ENVC_ACTIVE=\"", &script);
    c.done();
}

#[test]
fn an_unknown_shell_is_rejected() {
    let lab = Lab::new();
    let out = lab.run(&["--shell", "tcsh", "activate", "work"]);

    let mut c = Checks::default();
    c.is_true("未知 shell 应该报错", !out.status.success());
    c.has("并列出可选项", "expected bash, powershell or cmd", &stderr_of(&out));
    c.done();
}

// ===========================================================================
// 引号
// ===========================================================================
//
// 值中间出现的引号曾经被静默吃掉：`it's` 变成 `its`，`"double says 'hi'"`
// 连闭合引号都错位。这里把规则钉死：引号只在「值首或紧接闭合之后」才开启一段
// 引用，别的位置是普通字符；而引号内可以跨行。

#[test]
fn quotes_inside_a_value_survive() {
    let lab = Lab::new();
    lab.profile(
        "q",
        "A=it's\n\
         B=don't stop\n\
         C=\"double says 'hi'\"\n\
         D='literal'\n\
         E=a\"b\n\
         F=x'y'z\n\
         G=\"a\"'b'\n",
    );

    let out = lab.sh(
        r#"unset A B C D E F G
eval "$(envc activate q)"
printf "%s|%s|%s|%s|%s|%s|%s" "$A" "$B" "$C" "$D" "$E" "$F" "$G""#,
    );

    let mut c = Checks::default();
    c.eq(
        "值里的引号该原样保留",
        "it's|don't stop|double says 'hi'|literal|a\"b|x'y'z|ab",
        &out,
    );
    c.done();
}

#[test]
fn a_quoted_value_can_span_lines() {
    let lab = Lab::new();
    lab.profile(
        "q",
        "MULTI=\"first\nsecond\nthird\"\nSINGLE='raw\n$HOME stays'\nAFTER=ok\n",
    );

    let out = lab.sh(
        r#"unset MULTI SINGLE AFTER
eval "$(envc activate q)"
printf "%s|%s|%s" "$MULTI" "$SINGLE" "$AFTER""#,
    );

    let mut c = Checks::default();
    // 引号里的换行是值的一部分；单引号内连 $HOME 都不展开。
    // 末尾的 AFTER=ok 用来证明解析器正确接回了下一行。
    c.eq(
        "引号可以跨行",
        "first\nsecond\nthird|raw\n$HOME stays|ok",
        &out,
    );
    c.done();
}

#[test]
fn an_unterminated_quote_is_reported_with_its_line() {
    let lab = Lab::new();
    lab.profile("q", "A=1\nB=\"oops\nC=2\n");

    let out = lab.run(&["activate", "q"]);

    let mut c = Checks::default();
    c.is_true("未闭合的引号应该报错", !out.status.success());
    c.has("指出引号是从哪一行开始的", "q/.env:2", &stderr_of(&out));
    c.has("并说明是哪种引号", "unterminated \" quote", &stderr_of(&out));
    c.done();
}
