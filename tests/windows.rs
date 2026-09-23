//! windows: envc 在 Windows 上的集成测试，覆盖 cmd 与 PowerShell 两个 shell。
//!
//! 与 POSIX 最大的不同：`enable` 不写 rc 文件，而是把变量写进**用户级环境**
//! （注册表），这样 cmd、PowerShell 以及任何之后启动的进程都能看到；`activate`
//! 只影响当前会话，PowerShell 直接求值 stdout，cmd 没有 `eval`，所以代码落到
//! `%TEMP%\envc\activate.cmd`，由 `call` 执行。
//!
//! 注意：测 `enable`/`disable` 会真的动用户环境变量。变量名都带
//! `ENVC_T_<pid>_<序号>_` 前缀（序号是必须的：测试并行跑，共用一份注册表），
//! Drop 里还会兜底删一遍。但仍然是在改测试机自己的注册表——这是这个功能的性质
//! 决定的，绕不开。
#![cfg(target_os = "windows")]

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const ENVC: &str = env!("CARGO_BIN_EXE_envc");

struct Lab {
    root: PathBuf,
    /// 测试用变量的前缀，避免和用户自己的变量撞上。
    prefix: String,
}

impl Lab {
    fn new() -> Lab {
        static N: AtomicU32 = AtomicU32::new(0);
        // `cargo test` 并行跑，而这些测试改的是**同一份**用户环境：变量名必须
        // 每个测试一份，否则两个测试会互相把对方的值改掉。
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "envc-win-{}-{n}",
            std::process::id(),
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();

        Lab {
            root,
            prefix: format!("ENVC_T_{}_{n}_", std::process::id()),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn envc_home(&self) -> PathBuf {
        self.home().join(".envc")
    }

    /// cmd 的交接文件；`%TEMP%` 指向沙箱，所以它落在里面。
    fn batch(&self, what: &str) -> PathBuf {
        self.root.join("tmp").join("envc").join(format!("{what}.cmd"))
    }

    fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// 只覆盖这几个，其余**继承**。
    ///
    /// Windows 上不能像 Linux 那样把环境刮干净：SystemRoot、PATHEXT、ComSpec
    /// 这些没了，PowerShell 和 cmd 自己就起不来。变量名带 pid 和序号，继承来的
    /// 环境不会和测试用的撞上。
    fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("USERPROFILE", self.home().into()),
            ("HOME", self.home().into()),
            ("ENVC_HOME", self.envc_home().into()),
            ("TEMP", self.root.join("tmp").into()),
            ("TMP", self.root.join("tmp").into()),
        ]
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.envs(self.env())
            // 继承来的会被当成「这个 shell 已经装了什么」。
            .env_remove("ENVC_ACTIVE");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(ENVC).args(args).output().expect("envc 应该能启动")
    }

    /// 跑一段 PowerShell，返回 stdout。
    fn powershell(&self, script: &str) -> String {
        let out = self
            .command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .expect("powershell 应该能启动");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    /// cmd 里跑一段，返回 stdout。
    ///
    /// `/v:on` 打开延迟展开：cmd 在**解析整行**时就把 `%VAR%` 换掉了，所以
    /// `call x.cmd && echo %VAR%` 永远读不到 `x.cmd` 里刚 set 的值，必须写
    /// `!VAR!`。
    fn cmd(&self, script: &str) -> String {
        let out = self
            .command("cmd")
            .args(["/v:on", "/c", script])
            .output()
            .expect("cmd 应该能启动");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    /// 这个测试变量此刻在用户环境里的值。
    fn user_var(&self, key: &str) -> Option<String> {
        let got = self.powershell(&format!(
            "$v = [Environment]::GetEnvironmentVariable('{key}','User'); \
             if ($null -eq $v) {{ '' }} else {{ $v }}"
        ));
        if got.is_empty() {
            None
        } else {
            Some(got)
        }
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

/// 兜底：无论测试怎么结束，都不把测试变量留在用户环境里。
impl Drop for Lab {
    fn drop(&mut self) {
        for name in ["FOO", "BAR", "ENVC_ACTIVE", "PRE_EXISTING"] {
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
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 把一次运行的 stdout / stderr / 退出码都摊开——少了 stderr，失败就只剩猜。
fn describe(out: &Output) -> String {
    format!(
        "exit={:?}\n     stdout: {}\n     stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim_end(),
        String::from_utf8_lossy(&out.stderr).trim_end()
    )
}

fn checks(failures: Vec<String>) {
    assert!(failures.is_empty(), "\n{} 处失败:\n{}", failures.len(), failures.join("\n"));
}

// ===========================================================================
// 启动轴：enable / disable 写用户级环境变量
// ===========================================================================

#[test]
fn enable_writes_user_level_variables() {
    let lab = Lab::new();
    let foo = lab.key("FOO");
    // 第二个变量走一次环境展开，确认写进注册表的是展开后的值。
    let bar = lab.key("BAR");
    lab.profile("work", &format!("{foo}=from-work\n{bar}=$HOME\n"));

    let out = lab.run(&["enable", "work"]);

    let mut failures = Vec::new();
    if !out.status.success() {
        failures.push(format!("enable 应该成功:\n     {}", describe(&out)));
    }
    if lab.user_var(&foo).as_deref() != Some("from-work") {
        failures.push(format!("{foo} 该被写进用户环境，实际 {:?}", lab.user_var(&foo)));
    }
    if lab.read("startup").trim().is_empty() {
        failures.push("选择该被记在 ~/.envc/startup 里".to_string());
    }
    // 这一层的还原记录另存一份，别和会话栈混在一起。
    if lab.read("user-stack").is_empty() {
        failures.push("该记下旧值，好让 disable 能还回去".to_string());
    }
    checks(failures);
}

#[test]
fn disable_puts_the_user_variables_back() {
    let lab = Lab::new();
    let foo = lab.key("FOO");
    let bar = lab.key("BAR");
    lab.profile("work", &format!("{foo}=from-work\n{bar}=from-work\n"));

    lab.run(&["enable", "work"]);
    lab.run(&["disable"]);

    let mut failures = Vec::new();
    // 两个变量都在 enable 之前不存在，所以 disable 之后该被删掉，而不是留空。
    for key in [&foo, &bar] {
        if let Some(v) = lab.user_var(key) {
            failures.push(format!("{key} 该被删掉，实际还留着 {v:?}"));
        }
    }
    if lab.read("user-stack").trim().is_empty() {
        failures.push("user-stack 该被清掉".to_string());
    }
    // 选择仍然记着，裸 enable 能恢复。
    if !lab.read("startup").contains("work") {
        failures.push("disable 之后仍该记着原来的选择".to_string());
    }
    checks(failures);
}

#[test]
fn disable_restores_a_value_that_was_already_there() {
    let lab = Lab::new();
    let key = lab.key("PRE_EXISTING");
    lab.profile("work", &format!("{key}=from-work\n"));

    // 先让这个变量在用户级有个旧值。
    lab.powershell(&format!(
        "[Environment]::SetEnvironmentVariable('{key}','original','User')"
    ));

    lab.run(&["enable", "work"]);
    let during = lab.user_var(&key);
    lab.run(&["disable"]);
    let after = lab.user_var(&key);

    let mut failures = Vec::new();
    if during.as_deref() != Some("from-work") {
        failures.push(format!("enable 该盖掉旧值，实际 {during:?}"));
    }
    if after.as_deref() != Some("original") {
        failures.push(format!("disable 该还原成 original，实际 {after:?}"));
    }
    checks(failures);
}

#[test]
fn init_does_not_touch_a_shell_profile() {
    let lab = Lab::new();
    // Windows 上 $PROFILE 是 PowerShell 的，cmd 根本读不到；两边都不该被改。
    let profile = lab.home().join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1");

    let out = lab.run(&["init"]);
    let report = String::from_utf8_lossy(&out.stdout).to_string();

    let mut failures = Vec::new();
    if !out.status.success() {
        failures.push("init 应该成功".to_string());
    }
    if !report.contains("user environment") {
        failures.push(format!("init 该说明启动加载走用户环境:\n{report}"));
    }
    if profile.exists() {
        failures.push("init 不该去写 PowerShell 的 profile".to_string());
    }
    checks(failures);
}

// ===========================================================================
// 当前轴：activate / deactivate
// ===========================================================================

#[test]
fn powershell_evaluates_the_printed_code() {
    let lab = Lab::new();
    let foo = lab.key("FOO");
    lab.profile("work", &format!("{foo}=from-work\n"));

    let code = String::from_utf8_lossy(&lab.run(&["--shell", "powershell", "activate", "work"]).stdout)
        .to_string();

    let mut failures = Vec::new();
    if !code.contains(&format!("$env:{foo}='from-work'")) {
        failures.push(format!("该打印 PowerShell 语法:\n{code}"));
    }

    // 真的求值一遍，看变量有没有落到会话里。多行代码要用 `; ` 接起来：
    // 直接拼会在行尾留下 `\n;`，那不是合法的语句序列。
    let joined = code.lines().collect::<Vec<_>>().join("; ");
    let value = lab.powershell(&format!("{joined}; Write-Output $env:{foo}"));
    if value != "from-work" {
        failures.push(format!("求值之后该是 from-work，实际 {value:?}"));
    }
    checks(failures);
}

#[test]
fn cmd_runs_the_batch_file_it_writes() {
    let lab = Lab::new();
    let foo = lab.key("FOO");
    lab.profile("work", &format!("{foo}=from work\n"));

    let out = lab.run(&["--shell", "cmd", "activate", "work"]);
    let path = lab.batch("activate");

    let mut failures = Vec::new();
    // cmd 没有 eval：stdout 上不该有东西可求值。
    if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        failures.push("cmd 的 stdout 该是空的".to_string());
    }
    if !path.is_file() {
        failures.push(format!("该写出 {}", path.display()));
        checks(failures);
        return;
    }

    // 真正 call 一遍——这是 cmd 那条路唯一的端到端验证。
    let value = lab.cmd(&format!("call \"{}\" && echo !{}!", path.display(), foo));
    if value != "from work" {
        failures.push(format!("call 之后该是 from work，实际 {value:?}"));
    }
    checks(failures);
}

#[test]
fn cmd_deactivate_undoes_what_activate_did() {
    let lab = Lab::new();
    let foo = lab.key("FOO");
    lab.profile("work", &format!("{foo}=from-work\n"));

    lab.run(&["--shell", "cmd", "activate", "work"]);
    lab.run(&["--shell", "cmd", "deactivate"]);
    let path = lab.batch("deactivate");

    let mut failures = Vec::new();
    if !path.is_file() {
        failures.push(format!("该写出 {}", path.display()));
        checks(failures);
        return;
    }

    // 原本不存在的变量，撤销之后该是空的。
    let value = lab.cmd(&format!("call \"{}\" && echo [!{}!]", path.display(), foo));
    if value != "[]" {
        failures.push(format!("撤销之后该是空的，实际 {value:?}"));
    }
    checks(failures);
}

#[test]
fn the_shell_is_detected_from_the_environment() {
    let lab = Lab::new();
    lab.profile("work", "FOO=bar\n");

    // PowerShell 会给每一个会话导出 PSModulePath，cmd 不会。
    let ps = lab
        .command(ENVC)
        .args(["activate", "work"])
        .env("PSModulePath", r"C:\Modules")
        .output()
        .unwrap();
    let cmd = lab.command(ENVC).args(["activate", "work"]).output().unwrap();

    let mut failures = Vec::new();
    if !String::from_utf8_lossy(&ps.stdout).contains("$env:") {
        failures.push("有 PSModulePath 时该按 PowerShell 生成".to_string());
    }
    if !String::from_utf8_lossy(&cmd.stdout).trim().is_empty() {
        failures.push("没有 PSModulePath 时该按 cmd 走批处理文件".to_string());
    }
    checks(failures);
}
