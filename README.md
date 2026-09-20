# envc

按 profile 管理环境变量的命令行工具。一个 profile 就是一份 `.env`，可以随时切换，
切换时把被覆盖的旧值记进「恢复栈」，`deactivate` 时再原样退回去。

适合这些场景：

- 同一台机器上在几套 API key / 代理 / 镜像源之间来回切
- 给不同的项目配不同的 `PATH`、`EDITOR`、`GOPATH`
- 临时试一套环境变量，试完一条命令退回原样

支持 Linux（bash）和 macOS（zsh）。

---

## 安装

一键安装（下载对应平台的二进制，装好后自动执行 `envc init`）：

```bash
curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash
```

不想把脚本管道进 shell 的话：

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh)
```

从源码构建：

```bash
cargo build --release
install -Dm755 target/release/envc ~/.local/bin/envc
envc init
```

### 安装脚本的选项

```bash
bash install.sh --help
```

| 选项 | 作用 |
| --- | --- |
| `-v, --version <tag>` | 装指定版本，例如 `--version v0.1.0`；默认装最新 |
| `-b, --bin-dir <dir>` | 指定安装目录；Linux 默认 `~/.local/bin`，macOS 默认 `/usr/local/bin` |
| `--update` | 只在有新版本时才更新，已是最新则静默退出（可以放 cron） |
| `--no-init` | 装完不自动跑 `envc init` |
| `--uninstall` | 先摘掉 shell 钩子，再删掉二进制 |

### 自动更新

重新跑一遍安装命令就是更新：

```bash
curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash
```

要让它自己定时检查，加一条 cron：

```cron
# 每天凌晨 3 点检查一次更新
0 3 * * * curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash -s -- --update --no-init >/dev/null 2>&1
```

`--update` 会先比对版本号，已经是最新就直接退出，所以可以放心地反复调用：

```bash
bash install.sh --update --no-init    # 有新版才装
envc --version                        # 看当前版本
```

---

## 快速开始

```bash
# 1. 建一个 profile，会生成 ~/.envc/profiles/work/.env
envc create work

# 2. 编辑它（envc 不会去改这个文件，你自己用 vim/编辑器写）
vim ~/.envc/profiles/work/.env

# 3. 装好 shell 钩子（只需一次）
envc init

# 4. 开一个新 shell，或者现在就让钩子生效
source ~/.bashrc      # macOS/zsh 用 source ~/.zshrc

# 5. 切换
envc activate work    # 覆盖当前 shell 的环境变量
envc deactivate       # 退回切换之前的样子
```

`.env` 里就写普通的 `KEY=value`：

```bash
# ~/.envc/profiles/work/.env
ANTHROPIC_BASE_URL=https://api.example.com
ANTHROPIC_AUTH_TOKEN=sk-xxxxxx
EDITOR=vim
PATH="$HOME/.local/bin:$PATH"
```

---

## 命令

| 命令 | 别名 | 作用 |
| --- | --- | --- |
| `envc create <name>` | | 新建 profile 模板，已存在时需加 `--force` |
| `envc list` | `ls` | 列出所有 profile，标出当前选中的 |
| `envc delete <name>` | `rm` | 删除 profile；删正在用的要加 `--force` |
| `envc activate <name>` | `use` | 用这个 profile 覆盖当前 shell 的环境变量 |
| `envc deactivate` | `de`, `unuse` | 撤销，把旧值原样放回去 |
| `envc init` | | 检测登录 shell，把启动钩子注入对应 rc 文件 |
| `envc enable` | | 同上，但会强制重写钩子（升级后用） |
| `envc disable` | | 从 rc 文件里摘掉钩子 |
| `envc stack` | | 打印当前的恢复栈 |
| `envc status` | | 看当前状态：有没有装钩子、选中了谁、这个 shell 是什么 |
| `envc autoload` | | 内部命令，给 rc 钩子用的 |

全局选项：`-q, --quiet` 少说废话；`-h, --help`；`-V, --version`。

### create

```bash
envc create work             # 新建
envc create work --force     # 覆盖已有的
```

只负责建出一个空的 `.env`，之后怎么改是你的事。

### list

```bash
$ envc list
  PROFILE  VARS  ENV FILE
  dev         2  ~/.envc/profiles/dev/.env
* work        5  ~/.envc/profiles/work/.env

startup loading: enabled (~/.bashrc)
selected profile: work
```

`*` 是当前选中的 profile。`VARS` 那一列出现 `!` 表示这个 `.env` 解析失败
（具体行号会打到 stderr）。

### activate / deactivate

```bash
eval "$(envc activate work)"
eval "$(envc deactivate)"
```

装了 shell 钩子之后（`envc init`）就不用写 `eval` 了：

```bash
envc activate work
envc deactivate
```

原因见下面的「[为什么需要 eval](#为什么需要-eval)」。

`activate` 是**覆盖**：profile 里的每个变量都会盖掉当前值。
`deactivate` 是**删除并回退**：被覆盖的恢复原值，profile 新引入的直接 `unset`。

### init / enable / disable

`envc init` 会：

1. 从 `$SHELL` 猜出你的登录 shell（猜不到就按系统默认：macOS 是 zsh，Linux 是 bash）；
2. 找到对应的 rc 文件 —— zsh 是 `~/.zshrc`，bash 是 `~/.bashrc`；
3. 检查里面有没有已经注入过，没有才写入；**重复执行是安全的**。

```bash
$ envc init
shell:   bash (from $SHELL=/bin/bash)
rc file: ~/.bashrc
hook:    installed
```

`enable` / `disable` 是同一个开关的手动版本。`disable` 会把 rc 文件还原成注入之前
的样子（逐字节一致）：

```bash
envc disable     # 新 shell 不再自动加载
envc init        # 想再打开就跑这个
```

注入的内容长这样，被两行标记夹住，方便精确删除：

```bash
# >>> envc initialize >>>
# Managed by `envc enable` / `envc disable` -- do not edit this block by hand.
if command -v envc >/dev/null 2>&1; then
    envc() {
        case "${1:-}" in
            activate|use|deactivate|de|unuse)
                eval "$(ENVC_WRAPPED=1 command envc "$@")"
                ;;
            *)
                command envc "$@"
                ;;
        esac
    }
    eval "$(command envc autoload)"
fi
# <<< envc initialize <<<
```

它做两件事：定义一个 `envc` shell 函数（让你不用手写 `eval`），以及在每个新 shell
里重新应用当前选中的 profile。如果二进制不在 `PATH` 上，`if` 会让整段静默跳过，
不会污染你的 shell。

### stack / status

```bash
envc stack     # 打印恢复栈文件
envc status    # 一览：home、钩子状态、选中的 profile、当前 shell
```

`status` 还会提醒你「当前 shell 装的 profile」和「选中的 profile」不一致的情况。

---

## `.env` 语法

就是普通的 dotenv，按行读：

```bash
# 注释
KEY=value
export KEY=value                # export 前缀可省
QUOTED="hello world"            # 双引号：转义 + 变量展开
LITERAL='$HOME 不会被展开'       # 单引号：完全字面量
TRAILING=abc # 空格后的 # 是注释
NOT_A_COMMENT=abc#def           # 紧贴着的 # 是值的一部分
ROOT=/opt/app
BIN=$ROOT/bin                   # 展开同文件里前面定义的变量
FALLBACK=${UNSET_VAR:-默认值}    # 不存在时用默认值
PATH="$HOME/.local/bin:$PATH"   # 展开当前环境变量
```

所有赋值都会被导出（相当于 `export`），不需要写 `set -a`。

解析失败会直接报错并指出行号，**不会**只应用一半：

```bash
$ envc activate dev
envc: ~/.envc/profiles/dev/.env:3: expected `KEY=value`
```

出错时恢复栈保持原样，也不会有半截 shell 代码被 `eval`。

---

## 恢复栈

文件在 `~/.envc/stack`，纯文本，长得像 `git diff`：

```diff
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
-PROJECT
+PROJECT=work
-PATH=/usr/bin:/bin
+PATH=/home/zq/bin:/usr/bin:/bin
```

- `-` 行是**覆盖之前**的值，也就是 `deactivate` 时要放回去的东西。
- `+` 行是 profile 装上去的值。
- 没有 `=value` 的行表示「当时这个变量根本不存在」，所以撤销动作是 `unset`。

`deactivate` 从下往上回放 `-` 那一侧，然后把整个文件删掉。

### 连切两个 profile 会退回到哪？

退回到**最开始**那个 shell，而不是前一个 profile。恢复栈记录的是「任何 profile 生效
之前」的状态：

```bash
$ EDITOR=nano bash -c '
    eval "$(envc activate work)"    # EDITOR=vim
    eval "$(envc activate dev)"     # EDITOR=emacs
    echo "现在: $EDITOR"
    eval "$(envc deactivate)"
    echo "退回: $EDITOR"'
现在: emacs
退回: nano        # 不是 vim
```

实现方式是：切换时先把上一个 profile 从栈里「剥掉」，再拿剥干净的环境去记录新值，
所以 `A -> B -> deactivate` 落在 A 之前。

### 注意

- 恢复栈是**每台机器一份**的全局文件（`~/.envc/stack`）。多个 shell 同时用不同的
  profile 会互相覆盖栈文件 —— 这是有意的取舍，单个用户日常使用不会有问题。
- 如果你手动删掉 `~/.envc/stack`，`deactivate` 就没东西可回放了；它会退化成只
  清掉 `ENVC_ACTIVE` 标记并提示你。

---

## 为什么需要 eval

子进程**不可能**改父 shell 的环境变量 —— 这是 Unix 的硬限制，任何工具都绕不过去。
所以 `activate` / `deactivate` 不自己去改环境，而是把要执行的 shell 代码打到 stdout：

```bash
$ envc activate work
export EDITOR=vim
export PROJECT=work
export ENVC_ACTIVE=work

envc: this shell has NOT been changed yet -- the lines above must be evaluated:
envc:     eval "$(envc activate work)"
envc: run `envc enable` once to install a wrapper so `envc activate work` just works.
```

（提示只在 stdout 是终端时出现 —— 也就是你直接敲命令、输出没人接的时候。
`eval "$(...)"` 时 stdout 是管道，不会有这些废话。）

`envc init` 装的那个 shell 函数就是替你写 `eval`，这是推荐用法。

值的转义是逐个字符检查的：只有 `[A-Za-z0-9%+,-./:=@_^]` 之外的内容才会加引号，
单引号用 `'\''` 处理。测试里拿真的 bash 跑了一遍回环验证。

---

## 自动加载

`envc activate work` 会把选择**持久化**到恢复栈里，所以之后每个新 shell 的钩子都会
自动应用 `work`。`envc deactivate` 会清掉这个选择，新 shell 就干净了。

也就是：

| 操作 | 当前 shell | 新开的 shell |
| --- | --- | --- |
| `envc activate work` | 立刻生效 | 自动加载 work |
| `envc deactivate` | 立刻回退 | 什么都不加载 |
| `envc disable` | 不变 | 什么都不加载（钩子没了） |

新 shell 启动时记录的是「登录环境」的值，所以在新 shell 里 `deactivate` 一样能退回
到 profile 生效之前。

---

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `ENVC_HOME` | 数据目录，默认 `~/.envc` |
| `ENVC_RC` | 要注入的 rc 文件，覆盖自动检测（旧名 `ENVC_BASHRC` 仍可用） |
| `NO_COLOR` | 关掉彩色输出 |
| `ENVC_WRAPPED` | 内部用：告诉二进制它输出的代码正被 `eval`，不用打印提示 |

---

## 目录结构

```
~/.envc/
├── profiles/
│   ├── work/.env      <- 一个 profile 一份 .env
│   └── dev/.env
└── stack              <- 恢复栈（文本，git diff 风格）
```

---

## 开发

```bash
cargo build            # 构建
cargo test             # 35 个单元测试
bash tests/e2e.sh      # 51 项端到端检查，在真实 bash 里跑完整流程
cargo clippy --all-targets
```

端到端测试会把 `ENVC_HOME` 和 `ENVC_RC` 指到临时目录，不会碰你的真实配置。

### 发布

打 tag 即可，GitHub Actions 会构建 Linux / macOS（x86_64 + aarch64）四个二进制并
附到 release 上：

```bash
git tag v0.1.0
git push origin v0.1.0
```

产物是 `envc-v0.1.0-<target>.tar.gz`，另有一份去掉版本号的副本
`envc-<target>.tar.gz`，让 `/releases/latest/download/...` 这种固定链接能一直用。
同时发布 `SHA256SUMS`，安装脚本会自动校验。

---

## License

MIT
