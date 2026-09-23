//! macos: envc 在 macOS / zsh 上的集成测试，登录 shell 的启动文件是 ~/.zshrc。
//!
//! 这里只测 macOS 的**差异**部分：登录 shell 的检测、`.zshrc` 这个名字、钩子在
//! zsh 语法下成立、以及真 zsh 里走一遍 activate/deactivate。与系统无关的行为覆盖
//! 在 tests/linux.rs 里——那是同一份 envc 逻辑，不必在两个平台重复一遍。
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const ENVC: &str = env!("CARGO_BIN_EXE_envc");

struct Lab {
    root: PathBuf,
    /// $SHELL 的值；`None` 表示这个变量不存在（走 OS 默认）。
    shell: Option<String>,
}

impl Lab {
    fn new(shell: Option<&str>) -> Lab {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "envc-macos-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        symlink(ENVC, root.join("bin/envc")).unwrap();

        let lab = Lab {
            root,
            shell: shell.map(str::to_string),
        };
        fs::create_dir_all(lab.home()).unwrap();
        lab
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn envc_home(&self) -> PathBuf {
        self.home().join(".envc")
    }

    fn zshrc(&self) -> PathBuf {
        self.home().join(".zshrc")
    }

    /// 每个子进程都从这里取环境：不设 ENVC_RC，因为检测逻辑本身是被测对象。
    fn command(&self, program: &str) -> Command {
        let mut path = vec![self.root.join("bin")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));

        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", self.home())
            .env("ENVC_HOME", self.envc_home())
            .env("PATH", std::env::join_paths(path).unwrap())
            // 继承来的这两个会污染结果：「这个 shell 装了什么」和登录 shell。
            .env_remove("ENVC_ACTIVE")
            .env_remove("SHELL");
        if let Some(shell) = &self.shell {
            cmd.env("SHELL", shell);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(ENVC)
            .args(args)
            .output()
            .expect("envc 应该能启动")
    }

    /// 在一个真的 zsh 里跑脚本。
    fn zsh(&self, script: &str) -> String {
        let out = self.command("zsh").arg("-c").arg(script).output().expect("zsh 应该能启动");
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

fn two_profiles(lab: &Lab) {
    lab.profile("work", "EDITOR=vim\nPROJECT=work\n");
    lab.profile("alt", "EDITOR=emacs\nPROJECT=alt\n");
}

// ===========================================================================
// 登录 shell 的检测
// ===========================================================================

#[test]
fn a_zsh_login_shell_gets_the_hook_in_zshrc() {
    let lab = Lab::new(Some("/bin/zsh"));

    let report = String::from_utf8_lossy(&lab.run(&["init"]).stdout).to_string();
    let rc = fs::read_to_string(lab.zshrc()).unwrap_or_default();

    assert!(report.contains("shell:   zsh"), "报告里应该说 zsh:\n{report}");
    assert!(report.contains(".zshrc"), "报告里应该指向 .zshrc:\n{report}");
    assert!(
        rc.contains("# >>> envc initialize >>>"),
        "钩子该写进 ~/.zshrc:\n{rc}"
    );
}

#[test]
fn without_a_shell_variable_macos_defaults_to_zsh() {
    let lab = Lab::new(None);
    lab.run(&["init"]);

    let rc = fs::read_to_string(lab.zshrc()).unwrap_or_default();
    assert!(
        rc.contains("# >>> envc initialize >>>"),
        "$SHELL 不存在时 macOS 该默认 zsh"
    );
    assert!(
        !lab.home().join(".bashrc").exists(),
        "不该顺手也去写 .bashrc"
    );
}

// ===========================================================================
// 在真的 zsh 里
// ===========================================================================

#[test]
fn the_hook_is_valid_zsh_syntax() {
    let lab = Lab::new(Some("/bin/zsh"));
    lab.run(&["init"]);

    // -n 只解析不执行，否则钩子里的 eval 会去调二进制。
    let out = lab
        .command("zsh")
        .arg("-n")
        .arg(lab.zshrc())
        .output()
        .expect("zsh 应该能启动");

    assert!(
        out.status.success(),
        "钩子应当是合法的 zsh:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn activate_and_deactivate_round_trip_in_zsh() {
    let lab = Lab::new(Some("/bin/zsh"));
    two_profiles(&lab);

    let out = lab.zsh(
        r#"EDITOR=nano; export EDITOR
unset PROJECT
eval "$(envc activate work)"
printf "on=%s,%s " "$EDITOR" "$PROJECT"
eval "$(envc deactivate)"
printf "off=%s,%s" "$EDITOR" "${PROJECT-<unset>}""#,
    );

    assert_eq!(out, "on=vim,work off=nano,<unset>");
}

#[test]
fn a_new_zsh_autoloads_the_startup_profile() {
    let lab = Lab::new(Some("/bin/zsh"));
    two_profiles(&lab);
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);

    let out = lab.zsh(&format!(
        "source \"{}\"; printf \"%s|%s\" \"$PROJECT\" \"$ENVC_ACTIVE\"",
        lab.zshrc().display()
    ));

    assert_eq!(out, "work|work");
}

#[test]
fn the_two_halves_stay_independent_in_zsh() {
    let lab = Lab::new(Some("/bin/zsh"));
    two_profiles(&lab);
    lab.run(&["init"]);
    lab.run(&["enable", "work"]);

    // activate 换掉当前 zsh 里的东西，但启动选择不动。
    let current = lab.zsh(&format!(
        "source \"{}\"; envc activate alt >/dev/null 2>&1; printf \"%s\" \"$PROJECT\"",
        lab.zshrc().display()
    ));
    assert_eq!(current, "alt");
    assert!(
        lab.read("startup").contains("work"),
        "启动选择该还是 work：\n{}",
        lab.read("startup")
    );

    // 而新开的 zsh 依然加载 work。
    let fresh = lab.zsh(&format!(
        "source \"{}\"; printf \"%s\" \"$PROJECT\"",
        lab.zshrc().display()
    ));
    assert_eq!(fresh, "work");
}
