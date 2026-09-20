# envc

按 profile 管理环境变量。一个 profile 就是一份 `.env`，随时切换，`deactivate` 时把被覆盖的旧值原样退回去。

支持 Linux（bash）和 macOS（zsh）。

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
| 校验和 | [`SHA256SUMS`](https://github.com/Jabberwocky238/envc/releases/latest/download/SHA256SUMS) |

下下来 `chmod +x` 放进 `PATH` 就能用：

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu
chmod +x ~/.local/bin/envc
envc init
```

要固定版本，把 `latest/download/<名字>` 换成 `download/v0.1.1/envc-v0.1.1-<target>`，例如：

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/download/v0.1.1/envc-v0.1.1-x86_64-unknown-linux-gnu
```

## 使用方法

### 快速开始

```bash
envc create work          # 建 profile，生成 ~/.envc/profiles/work/.env
vim ~/.envc/profiles/work/.env   # 用你自己的编辑器写
envc init                 # 装 shell 钩子，只需一次
source ~/.bashrc          # 让钩子马上生效（zsh 是 source ~/.zshrc）

envc activate work        # 覆盖当前 shell 的环境变量
envc deactivate           # 退回切换之前的样子
```

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
| `envc list` | `ls` | 列出所有 profile，`*` 标出当前选中的 |
| `envc delete <name>` | `rm` | 删除 profile，删正在用的要加 `--force` |
| `envc activate <name>` | `use` | 用这个 profile 覆盖当前 shell 的环境变量 |
| `envc deactivate` | `de`, `unuse` | 撤销，旧值原样放回去 |
| `envc init` | | 检测登录 shell，把钩子注入对应 rc 文件（可重复执行） |
| `envc enable` / `disable` | | 手动装 / 摘钩子 |
| `envc stack` | | 打印恢复栈 |
| `envc status` | | 看当前状态 |

全局选项：`-q, --quiet`、`-h, --help`、`-V, --version`。

`activate` 是覆盖：profile 里每个变量都会盖掉当前值。
`deactivate` 是回退：被覆盖的恢复原值，profile 新引入的直接 `unset`。
连切两个 profile 再 `deactivate`，回到的是最开始那个 shell，而不是前一个 profile。

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
TRAILING=abc # 空格后的 # 是注释
NOT_A_COMMENT=abc#def           # 紧贴着的 # 是值的一部分
ROOT=/opt/app
BIN=$ROOT/bin                   # 展开同文件里前面定义的变量
FALLBACK=${UNSET_VAR:-默认值}    # 不存在时用默认值
PATH="$HOME/.local/bin:$PATH"   # 展开当前环境变量
```

所有赋值都会被导出，不用写 `set -a`。解析失败会报错并指出行号，不会只应用一半。
