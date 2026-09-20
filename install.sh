#!/usr/bin/env bash
#
# envc installer and updater.
#
#   curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh | bash
#
# or, if you prefer not to pipe into a shell:
#
#   bash <(curl -fsSL https://raw.githubusercontent.com/Jabberwocky238/envc/main/install.sh)
#
# Running it again is the update path: it downloads the newest release and
# replaces the binary in place. Pass --update to make it a no-op when the
# installed version already matches, so it is safe to run from cron.
#
set -euo pipefail

REPO="${ENVC_REPO:-Jabberwocky238/envc}"
BIN="envc"

VERSION=""
BIN_DIR=""
DO_INIT=1
UPDATE_ONLY=0
DO_UNINSTALL=0

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    C_BOLD=$'\033[1m' C_DIM=$'\033[2m' C_RED=$'\033[31m' C_GREEN=$'\033[32m' C_RESET=$'\033[0m'
else
    C_BOLD= C_DIM= C_RED= C_GREEN= C_RESET=
fi

die() { printf '%serror:%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }
warn() { printf '%swarning:%s %s\n' "$C_DIM" "$C_RESET" "$*" >&2; }
step() { printf '%s==>%s %s\n' "$C_BOLD" "$C_RESET" "$*"; }
ok() { printf '%s  ok%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }

usage() {
    cat <<EOF
${C_BOLD}envc installer${C_RESET}

USAGE:
    install.sh [OPTIONS]

OPTIONS:
    -v, --version <tag>   Install a specific release (e.g. v0.1.0)
                          Default: the newest release
    -b, --bin-dir <dir>   Where to put the binary
                          Default: ~/.local/bin on Linux, /usr/local/bin on macOS
        --update          Update only if a newer release exists; quiet no-op otherwise
        --no-init         Do not run \`envc init\` afterwards
        --uninstall       Remove the shell hook and delete the binary
    -h, --help            Show this help

ENVIRONMENT:
    ENVC_REPO       GitHub repo to install from  (default: $REPO)
    ENVC_BIN_DIR    Same as --bin-dir
    NO_COLOR        Disable colored output
EOF
}

# ---------------------------------------------------------------------------
# arguments

while [ $# -gt 0 ]; do
    case "$1" in
        -v|--version)
            [ $# -ge 2 ] || die "$1 needs a tag, e.g. --version v0.1.0"
            VERSION="$2"
            shift 2
            ;;
        --version=*)
            VERSION="${1#*=}"
            [ -n "$VERSION" ] || die "--version needs a tag, e.g. --version=v0.1.0"
            shift
            ;;
        -b|--bin-dir)
            [ $# -ge 2 ] || die "$1 needs a directory"
            BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            [ -n "$BIN_DIR" ] || die "--bin-dir needs a directory"
            shift
            ;;
        --update) UPDATE_ONLY=1; shift ;;
        --no-init) DO_INIT=0; shift ;;
        --uninstall) DO_UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

[ -n "$BIN_DIR" ] || BIN_DIR="${ENVC_BIN_DIR:-}"

# ---------------------------------------------------------------------------
# platform

detect_target() {
    local os arch
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64) echo "x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) die "no prebuilt binary for Linux/$arch; build from source with 'cargo build --release'" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                *) die "no prebuilt binary for macOS/$arch" ;;
            esac
            ;;
        *)
            die "no prebuilt binary for $os; on Windows use WSL, or build from source" ;;
    esac
}

default_bin_dir() {
    if [ "$(uname -s)" = "Darwin" ]; then
        # /usr/local/bin is the conventional spot on macOS, but it is not
        # always writable by the user.
        if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
            echo "/usr/local/bin"
            return
        fi
    fi
    echo "$HOME/.local/bin"
}

need() { command -v "$1" >/dev/null 2>&1; }

download() { # <url> <dest>
    if need curl; then
        curl -fsSL --retry 3 --retry-delay 1 -o "$2" "$1"
    elif need wget; then
        wget -q -O "$2" "$1"
    else
        die "neither curl nor wget is available"
    fi
}

try_download() { # <url> <dest> -- returns non-zero instead of exiting
    if need curl; then
        curl -fsSL --retry 2 -o "$2" "$1" 2>/dev/null
    elif need wget; then
        wget -q -O "$2" "$1" 2>/dev/null
    else
        die "neither curl nor wget is available"
    fi
}

sha256_of() {
    if need sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif need shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo ""
    fi
}

latest_tag() {
    need curl || return 1
    curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null \
        | sed -n 's#.*/tag/##p'
}

installed_version() {
    local exe="$BIN_DIR/$BIN"
    [ -x "$exe" ] || return 1
    "$exe" --version 2>/dev/null | awk '{print $2}'
}

# ---------------------------------------------------------------------------
# uninstall

if [ "$DO_UNINSTALL" = 1 ]; then
    [ -n "$BIN_DIR" ] || BIN_DIR=$(default_bin_dir)
    exe="$BIN_DIR/$BIN"
    if [ -x "$exe" ]; then
        step "removing the shell hook"
        "$exe" disable 2>/dev/null || warn "could not run '$exe disable'; remove the envc block from your rc file by hand"
    fi
    if [ -e "$exe" ]; then
        step "removing $exe"
        rm -f "$exe"
        ok "uninstalled"
    else
        ok "nothing to remove at $exe"
    fi
    printf '%s\n' "Your profiles in ~/.envc were left alone. Remove that directory to delete them."
    exit 0
fi

# ---------------------------------------------------------------------------
# resolve what to install

TARGET=$(detect_target)
[ -n "$BIN_DIR" ] || BIN_DIR=$(default_bin_dir)

if [ -z "$VERSION" ]; then
    VERSION=$(latest_tag) || true
fi
TAG="${VERSION:-latest}"

step "platform ${C_BOLD}$TARGET${C_RESET}, release ${C_BOLD}$TAG${C_RESET}"

if [ "$UPDATE_ONLY" = 1 ] && [ -n "$VERSION" ]; then
    if current=$(installed_version) && [ "v$current" = "$VERSION" ]; then
        ok "envc $current is already up to date"
        exit 0
    fi
fi

if [ -n "$VERSION" ]; then
    ASSET="${BIN}-${VERSION}-${TARGET}"
    URL="https://github.com/$REPO/releases/download/${VERSION}/${ASSET}"
    SUMS_URL="https://github.com/$REPO/releases/download/${VERSION}/SHA256SUMS"
else
    # No tag known (no curl, or the API was unreachable): the version-less
    # asset alias published with every release still works.
    ASSET="${BIN}-${TARGET}"
    URL="https://github.com/$REPO/releases/latest/download/${ASSET}"
    SUMS_URL="https://github.com/$REPO/releases/latest/download/SHA256SUMS"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

step "downloading $ASSET"
try_download "$URL" "$TMP/$BIN" \
    || die "could not download $URL
       check that a release named '$TAG' exists: https://github.com/$REPO/releases"

# ---------------------------------------------------------------------------
# verify

if try_download "$SUMS_URL" "$TMP/SHA256SUMS"; then
    expected=$(awk -v f="$ASSET" '$2 == f {print $1}' "$TMP/SHA256SUMS")
    actual=$(sha256_of "$TMP/$BIN")
    if [ -z "$actual" ]; then
        warn "no sha256 tool found, skipping checksum verification"
    elif [ -z "$expected" ]; then
        warn "$ASSET is not listed in SHA256SUMS, skipping checksum verification"
    elif [ "$expected" != "$actual" ]; then
        die "checksum mismatch for $ASSET
       expected $expected
       got      $actual"
    else
        ok "checksum verified"
    fi
else
    warn "could not fetch SHA256SUMS, skipping checksum verification"
fi

# A 404 page is served with a 200 by some proxies; make sure we got an actual
# executable rather than HTML.
magic=$(head -c 4 "$TMP/$BIN" 2>/dev/null | od -An -tx1 | tr -d ' \n')
case "$magic" in
    7f454c46 | cffaedfe | cefaedfe | cafebabe) ;;
    *) die "$ASSET did not download as an executable (magic: ${magic:-empty})
       fetch it yourself: $URL" ;;
esac
chmod +x "$TMP/$BIN"

step "installing to $BIN_DIR"
mkdir -p "$BIN_DIR" 2>/dev/null || true
if [ ! -w "$BIN_DIR" ]; then
    die "$BIN_DIR is not writable
       re-run with --bin-dir ~/.local/bin, or put it there yourself:
           install -m755 $TMP/$BIN $BIN_DIR/$BIN"
fi

# install(1) writes the replacement in place rather than truncating the
# running binary, which keeps upgrades safe.
if need install; then
    install -m 755 "$TMP/$BIN" "$BIN_DIR/$BIN"
else
    cp -f "$TMP/$BIN" "$BIN_DIR/$BIN"
    chmod 755 "$BIN_DIR/$BIN"
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        warn "$BIN_DIR is not on your PATH; add it:"
        printf '         export PATH="%s:$PATH"\n' "$BIN_DIR"
        ;;
esac

installed=$("$BIN_DIR/$BIN" --version 2>/dev/null || echo "$BIN")
ok "installed $installed"

# ---------------------------------------------------------------------------
# shell integration

if [ "$DO_INIT" = 1 ] && [ "$UPDATE_ONLY" = 0 ]; then
    step "setting up shell integration"
    if "$BIN_DIR/$BIN" init; then
        :
    else
        warn "could not install the startup hook; run '$BIN init' yourself to see why"
    fi
else
    printf '\nNext: %s init%s sets up the shell hook.\n' "$BIN" "$C_RESET"
fi
