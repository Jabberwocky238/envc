# envc

Environment variables managed as profiles. A profile is one `.env` file. Profiles stack up in the
current shell: `use` pushes one, `unuse` takes it off again and puts back exactly what it covered.

Runs on Linux (bash), macOS (zsh) and Windows (cmd and PowerShell).

## Install

One line. It detects the platform and runs `envc init` when it is done:

```bash
curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash
```

Without piping a script into a shell:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh)
```

Or download a binary ([all releases](https://github.com/Jabberwocky238/envc/releases)):

| Platform | Download |
| --- | --- |
| Linux x86_64 | [`envc-x86_64-unknown-linux-gnu`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu) |
| Linux arm64 | [`envc-aarch64-unknown-linux-gnu`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-unknown-linux-gnu) |
| macOS Intel | [`envc-x86_64-apple-darwin`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-apple-darwin) |
| macOS Apple Silicon | [`envc-aarch64-apple-darwin`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-apple-darwin) |
| Windows x64 | [`envc-x86_64-pc-windows-msvc.exe`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-pc-windows-msvc.exe) |
| Windows arm64 | [`envc-aarch64-pc-windows-msvc.exe`](https://github.com/Jabberwocky238/envc/releases/latest/download/envc-aarch64-pc-windows-msvc.exe) |
| Checksums | [`SHA256SUMS`](https://github.com/Jabberwocky238/envc/releases/latest/download/SHA256SUMS) |

On Linux and macOS, `chmod +x` it and put it on `PATH`. On Windows, putting the `.exe` on `PATH` is enough.

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/latest/download/envc-x86_64-unknown-linux-gnu
chmod +x ~/.local/bin/envc
envc init
```

To pin a version, replace `latest/download/<name>` with `download/v0.3.0/envc-v0.3.0-<target>`:

```bash
curl -fsSL -o ~/.local/bin/envc https://github.com/Jabberwocky238/envc/releases/download/v0.3.0/envc-v0.3.0-x86_64-unknown-linux-gnu
```

### Behind a slow link to GitHub

Put a GitHub mirror prefix in front of every GitHub URL:

```bash
curl -fsSL https://gh-proxy.com/https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh \
    | ENVC_GH_PROXY=https://gh-proxy.com/ bash
```

`ENVC_GH_PROXY` is needed: it routes the binary download *inside* the script through the mirror as
well. The script checks `SHA256SUMS`, so a mirror that truncates the binary fails with
`checksum mismatch` instead of installing a broken one.

Once installed, `envc` itself never touches the network.

## Usage

### Quick start

```bash
envc create work                 # creates ~/.envc/profiles/work/.env
vim ~/.envc/profiles/work/.env   # edit it with anything
envc init                        # install the shell hook, once
source ~/.bashrc                 # or ~/.zshrc

envc enable work                 # every new shell loads work
envc use work                    # push work onto this shell
envc unuse work                  # take it off again
```

A `.env` is plain `KEY=value`:

```bash
ANTHROPIC_BASE_URL=https://api.example.com
ANTHROPIC_AUTH_TOKEN=sk-xxxxxx
EDITOR=vim
PATH="$HOME/.local/bin:$PATH"
```

### Commands

| Command | Alias | What it does |
| --- | --- | --- |
| `envc create <profile>` | | Create a profile; `--force` to overwrite |
| `envc list` | `ls` | List profiles; `*` marks the startup one |
| `envc delete <profile>` | `rm` | Delete a profile; `--force` if it is in use or the startup one |
| `envc use <profile>` | `push` | Push a profile onto **this shell's** stack |
| `envc unuse <profile>` | `pop` | Take that profile off the stack, restoring what it covered |
| `envc enable <profile>` | | Make new shells load this profile, installing the hook if needed |
| `envc disable <profile>` | | Stop new shells loading it (the current shell is left alone) |
| `envc init` | | Detect the login shell and inject the hook into its rc file (safe to re-run) |
| `envc stack` | | Print this shell's stack |
| `envc status` | | Show what is enabled and in use |

Global options: `-q, --quiet`, `--shell <bash|powershell|cmd>`, `-h, --help`, `-V, --version`.
`use` and `unuse` also take `-y, --yes` (see [Changed profiles](#changed-profiles)).

### The stack

Every `use` pushes a frame recording, for each variable the profile sets, the value it had before
and the value the profile installed. `unuse <profile>` takes that frame out:

```bash
envc use a        # base > a
envc use b        # base > a > b
envc unuse a      # base > b
```

`unuse` peels every frame from the top down to `a`, replaying the old values, then re-applies the
frames that were above it (`b`) on the restored base. A variable that `a` and `b` both set ends up
with `b`'s value. Anything in `b` that expanded a variable of `a` is re-expanded without `a`. The
result is exactly the shell you would have had if `a` had never been used.

A profile can only be on the stack once. `unuse` always needs a name, so you say what you are
taking off.

### One stack per shell

Each shell keeps its own stack in `~/.envc/stack/<session>`. On Linux and macOS the session is the
terminal's session id. On Windows it is the shell's process id. A shell also exports
`ENVC_ACTIVE` (the stack, as `a:b`), and envc only trusts a stack file that matches it, so a file
left behind by a dead session is never applied to a new one.

Nested shells in the same terminal share a session. If one of them changes the stack, the other
gets a warning and ignores it.

### Changed profiles

A frame remembers a fingerprint of the `.env` it was pushed from. Before `use` (every frame) and
`unuse` (the frames it re-applies), envc compares each one with the file on disk. If any changed,
it lists the variables that would be left dangling, changed or newly added, and asks:

```
envc: ~/.envc/profiles/a/.env changed since it was pushed
envc:     dangling EV_ONLY_A
envc:     changed  EV_SHARED
envc: continue anyway? [y/N]
```

Anything other than `y` aborts with nothing changed. `-y` answers yes.

### History and snapshots

- `~/.envc/snapshots/<profile>/<fingerprint>.env` keeps a copy of every `.env` version that was
  actually applied.
- `~/.envc/history` records every `push`, `pop`, `repush` and `autoload`, with the time, the
  session, the profile version and the full restore snapshot.

### Two halves, kept apart

| | Commands | State |
| --- | --- | --- |
| What new shells load | `enable` / `disable` | `~/.envc/startup` |
| What this shell has | `use` / `unuse` | `~/.envc/stack/<session>` |

`use` never changes the startup choice, and `enable` never touches the shell you are in.
`envc status` shows both and points out when they disagree.

### Windows

No startup file is read by both cmd and PowerShell, so the startup half works differently:

| | On Windows |
| --- | --- |
| `enable` / `disable` | `enable` writes the profile's variables into the **user environment** (`[Environment]::SetEnvironmentVariable(..., 'User')`). Every program started afterwards sees them. `disable` puts back what they held before, and removes the ones that did not exist. The old values live in `~/.envc/user-stack`. |
| `use` / `unuse` | Current session only. PowerShell evaluates stdout. cmd has no `eval`, so the code goes to `%TEMP%\envc\use.cmd` (or `unuse.cmd`) for you to `call`. |

```powershell
envc use work   | Invoke-Expression
envc unuse work | Invoke-Expression
```

```bat
envc use work   && call "%TEMP%\envc\use.cmd"
envc unuse work && call "%TEMP%\envc\unuse.cmd"
```

Which syntax to emit is guessed from the environment (PowerShell exports `PSModulePath` to every
session, cmd does not). Override it with `--shell`, or `shell` in the config file.

### About eval

A child process cannot change its parent shell, so `use` and `unuse` print shell code on stdout.
With the hook from `envc init` you just type the command. Without it:

```bash
eval "$(envc use work)"
eval "$(envc unuse work)"
```

### Config

`~/.envc/config`, one `key = value` per line. Every setting is optional:

```
shell = bash
rc = ~/.bashrc
color = auto
```

| Key | Values | Default |
| --- | --- | --- |
| `shell` | `bash`, `powershell`, `cmd` | guessed from the environment |
| `rc` | the rc file `init` / `enable` / `disable` patch | `~/.bashrc` or `~/.zshrc`, from `$SHELL` |
| `color` | `auto`, `always`, `never` | `auto` (color only on a terminal) |

An unknown key is an error that names its line.

### `.env` syntax

```bash
# comment
KEY=value
export KEY=value                # the export prefix is optional
QUOTED="hello world"            # double quotes: escapes and expansion
LITERAL='$HOME stays literal'   # single quotes: fully literal
DONT_PANIC=it's                 # a quote in the middle of a value is an ordinary character
MULTI="line one
line two"                       # a newline inside quotes is part of the value
TRAILING=abc # a # after a space starts a comment
NOT_A_COMMENT=abc#def           # a # stuck to the value is part of it
ROOT=/opt/app
BIN=$ROOT/bin                   # expands variables defined earlier in the file
FALLBACK=${UNSET_VAR:-default}  # default when unset or empty
PATH="$HOME/.local/bin:$PATH"   # expands the current environment
```

Two quoting rules, both there so that nothing you typed is silently eaten:

- **A quote only opens a quoted section at the start of the value, or straight after another
  section closed.** Anywhere else it is an ordinary character, so `it's`, `a"b` and `x'y'z` survive
  as typed, while `"a"'b'` concatenates to `ab`.
- **A quoted section may span lines.** The newline is part of the value. Inside single quotes not
  even `$HOME` expands.

Every assignment is exported. A parse error names the file and line (for an unterminated quote,
the line it opened on) and nothing is applied.
