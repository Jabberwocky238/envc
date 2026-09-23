# envc

按 profile 管理环境变量。一个 profile 就是一份 `.env`，随时切换，`deactivate` 时把被覆盖的旧值原样退回去。

支持 Linux（bash）、macOS（zsh）和 Windows（cmd 与 PowerShell）。

## 下载安装

一键安装，自动识别平台，装好顺手跑 `envc init`：

```bash
curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash
```

不想把脚本管道进 shell：

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh)
```

直接下载二进制（[全部版本](https://github.com/Jabberwocky238/envc/releases)）：

| 平台 | 下载 |
| --- | --- |
| Linux x86_64 | [`envc-x86_64-unknown-linux-gnu`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu) |
| Linux arm64 | [`envc-aarch64-unknown-linux-gnu`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-unknown-linux-gnu) |
| macOS Intel | [`envc-x86_64-apple-darwin`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-apple-darwin) |
| macOS Apple Silicon | [`envc-aarch64-apple-darwin`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-apple-darwin) |
| Windows x64 | [`envc-x86_64-pc-windows-msvc.exe`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-pc-windows-msvc.exe) |
| Windows arm64 | [`envc-aarch64-pc-windows-msvc.exe`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-pc-windows-msvc.exe) |
| 校验和 | [`SHA256SUMS`](https://github.com/Jabberwocky238/envc/releases/latest/download/SHA256SUMS) |

（Windows 上把 `.exe` 放进 `PATH` 即可，不用 `chmod`。）

下下来 `chmod +x` 放进 `PATH` 就能用：

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu
chmod +x ~/.local/bin/envc
envc init
```

要固定版本，把 `latest/download/<名字>` 换成 `download/v0.2.0/envc-v0.2.0-<target>`，例如：

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/download/v0.2.0/envc-v0.2.0-x86_64-unknown-linux-gnu
```

### 中国大陆加速

直连 GitHub 常常超时，可以在所有 GitHub URL 前面加一层 ghproxy 反代：

```bash
curl -fsSL https://gh-proxy.com/https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh \
    | ENVC_GH_PROXY=https://gh-proxy.com/ bash
```

`ENVC_GH_PROXY` 不能省：它让**脚本内部**的二进制下载也走镜像。只给脚本本身套反代是不够的，脚本里的下载地址默认直连 `github.com`，国内照样会卡住。

镜像域名经常换，`gh-proxy.com` 是写这份文档时实测可用的，换别的就把前缀整个替换掉。不同镜像质量差别很大，有的会把二进制截断——别担心，脚本会比对 `SHA256SUMS`，对不上会直接 `checksum mismatch` 报错退出，不会装上一个坏掉的二进制。

手动下载也一样，在表里的 URL 前面加前缀即可：

```bash
curl -fsSL -o ~/.local/bin/envc https://gh-proxy.com/https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu
chmod +x ~/.local/bin/envc
```

装完之后 `envc` 本体不再联网（它只读写 `~/.envc` 和 rc 文件），所以这个变量只在安装、或者日后用 `install.sh` 升级时才需要给。

## 使用方法

### 快速开始

```bash
envc create work          # 建 profile，生成 ~/.envc/profiles/work/.env
vim ~/.envc/profiles/work/.env   # 用你自己的编辑器写
envc init                 # 装 shell 钩子，只需一次
source ~/.bashrc          # 让钩子马上生效（zsh 是 source ~/.zshrc）

envc enable work          # 以后每个新 shell 都加载 work
envc activate work        # 只想改当前 shell 的话，用这个
envc deactivate           # 退回切换之前的样子
```

（Windows 上没有 rc 文件那一步，也不需要 `envc init`，见下面的 [Windows](#windows) 一节。）

`.env` 就是普通的 `KEY=value`：

```bash
# ~/.envc/profiles/work/.env
ANTHROPIC_BASE_URL=https://api.example.com
ANTHROPIC_AUTH_TOKEN=sk-xxxxxx
EDITOR=vim
PATH="$HOME/.local/bin:$PATH"
```

### 命令

| 命令 | 别名 | 作用 |
| --- | --- | --- |
| `envc create <name>` | | 新建 profile，已存在要加 `--force` |
| `envc list` | `ls` | 列出所有 profile，`*` 标出启动加载的那个 |
| `envc delete <name>` | `rm` | 删除 profile，删启动中/正在用的要加 `--force` |
| `envc enable [name]` | | 定下新 shell 加载哪个 profile（不给名字就沿用上次的），并确保钩子已装 |
| `envc disable` | | 新 shell 不再加载任何 profile（当前 shell 不动，选择仍记着） |
| `envc activate <name>` | `use` | 用这个 profile 覆盖**当前 shell** 的环境变量 |
| `envc deactivate` | `de`, `unuse` | 撤销，旧值原样放回去 |
| `envc init` | | 检测登录 shell，把钩子注入对应 rc 文件（可重复执行） |
| `envc stack` | | 打印恢复栈 |
| `envc status` | | 看当前状态 |

全局选项：`-q, --quiet`、`--shell <bash|powershell|cmd>`、`-h, --help`、`-V, --version`。
`--shell` 也可以用环境变量 `ENVC_SHELL` 给；不给就按平台和当前环境猜。

`activate` 是覆盖：profile 里每个变量都会盖掉当前值。
`deactivate` 是回退：被覆盖的恢复原值，profile 新引入的直接 `unset`。
连切两个 profile 再 `deactivate`，回到的是最开始那个 shell，而不是前一个 profile。

### 两件事，别混

envc 管的是两个互相独立的问题，命令也对半分：

| | 谁管 | 状态存哪 |
| --- | --- | --- |
| 新 shell 启动加载哪个 profile | `enable` / `disable` | `~/.envc/startup` |
| 当前这个 shell 加载了什么 | `activate` / `deactivate` | `~/.envc/stack`（恢复栈） |

所以：

```bash
envc enable work     # 新 shell 从此加载 work
envc activate other  # 当前 shell 换成 other
                     # 新开的 shell 依然是 work
```

`activate` 改不动启动项，`enable` 也不会碰你手上这个 shell（它会提示你用 `activate`）。
`disable` 只关掉启动加载，仍然记着之前选的是谁，裸跑 `envc enable` 就能原样恢复。

`envc status` 会把两边并排打出来，两边不一致时会提醒你。

### Windows

Windows 上没有一份 cmd 和 PowerShell 都会读的启动文件，所以两个轴换了个实现：

| | Windows 上怎么做 |
| --- | --- |
| 启动轴（`enable` / `disable`） | `enable` 把 profile 里的变量写进**用户级环境变量**（经 PowerShell 的 `[Environment]::SetEnvironmentVariable(...,'User')`）。这是永久的，之后启动的任何程序——cmd、PowerShell、别的——都看得到。`disable` 会把覆盖前的旧值还原回去，之前不存在的就删掉。 |
| 当前轴（`activate` / `deactivate`） | 仍然只影响当前会话。PowerShell 直接求值 stdout；cmd 没有 `eval`，所以代码写到 `%TEMP%\envc\activate.cmd`，由你 `call` 它。 |

```powershell
# PowerShell
envc activate work | Invoke-Expression
envc deactivate      | Invoke-Expression
```

```bat
:: cmd
envc activate work && call "%TEMP%\envc\activate.cmd"
envc deactivate   && call "%TEMP%\envc\deactivate.cmd"
```

用户在 `enable` 之前就有同名变量的话，`disable` 会把它还原成原来的值（而不是删掉）——旧值记在 `~/.envc/user-stack`，和会话用的 `~/.envc/stack` 分开。

生成哪种 shell 的语法是按环境猜的（PowerShell 会给每个会话导出 `PSModulePath`，cmd 不会）。猜错了或者要在别处用，用 `--shell` 指定：

```bash
envc --shell cmd activate work
envc --shell powershell activate work
```

### 关于 eval

子进程改不了父 shell 的环境变量，所以 `activate` / `deactivate` 把要执行的 shell 代码打到 stdout。装了钩子（`envc init`）之后直接敲就行，没装钩子要自己套一层 `eval`：

```bash
eval "$(envc activate work)"
eval "$(envc deactivate)"
```

### `.env` 写法

```bash
# 注释
KEY=value
export KEY=value                # export 前缀可省
QUOTED="hello world"            # 双引号：转义 + 变量展开
LITERAL='$HOME 不展开'           # 单引号：完全字面量
DONT_PANIC=it's                 # 值中间的引号是普通字符，原样保留
MULTI="第一行
第二行"                          # 引号内的换行也是值的一部分
TRAILING=abc # 空格后的 # 是注释
NOT_A_COMMENT=abc#def           # 紧贴着的 # 是值的一部分
ROOT=/opt/app
BIN=$ROOT/bin                   # 展开同文件里前面定义的变量
FALLBACK=${UNSET_VAR:-默认值}    # 不存在时用默认值
PATH="$HOME/.local/bin:$PATH"   # 展开当前环境变量
```

引号的规则只有两条，都是为了避免把你打的字符悄悄吃掉：

- **引号只在「值开头，或紧接另一段引号结束之后」才开始一段引用**；出现在别处就是
  普通字符。所以 `it's`、`a"b`、`x'y'z` 都原样保留，`"a"'b'` 则拼接成 `ab`。
- **一段引用可以跨行**，引号内的换行是值的一部分。单引号里连 `$HOME` 都不展开。

所有赋值都会被导出，不用写 `set -a`。解析失败会报错并指出行号——包括引号没闭合，
报的是引号开始的那一行——不会只应用一半。
