#!/usr/bin/env bash
#
# End-to-end test: drives the built binary from real shells against a throwaway
# ENVC_HOME and a throwaway rc file. Nothing outside the temp directory is
# touched.
#
#   cargo build && bash tests/e2e.sh
#
set -uo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ENVC_BIN=${ENVC_BIN:-$REPO_ROOT/target/debug/envc}

if [ ! -x "$ENVC_BIN" ]; then
    echo "e2e: $ENVC_BIN not found -- run 'cargo build' first" >&2
    exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Everything envc touches is redirected into the sandbox.
mkdir -p "$TMP/bin"
ln -sf "$ENVC_BIN" "$TMP/bin/envc"
export PATH="$TMP/bin:$PATH"
export ENVC_HOME="$TMP/.envc"
export ENVC_RC="$TMP/.bashrc"

FAILED=0
PASSED=0

check() { # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        PASSED=$((PASSED + 1))
        printf '  ok   %s\n' "$1"
    else
        FAILED=$((FAILED + 1))
        printf '  FAIL %s\n       expected: %s\n       actual:   %s\n' "$1" "$2" "$3"
    fi
}

check_contains() { # <description> <needle> <haystack>
    case "${3-}" in
        *"$2"*)
            PASSED=$((PASSED + 1))
            printf '  ok   %s\n' "$1"
            ;;
        *)
            FAILED=$((FAILED + 1))
            printf '  FAIL %s\n       %s not found in:\n%s\n' "$1" "$2" "${3-}"
            ;;
    esac
}

section() { printf '\n== %s\n' "$1"; }

# Run envc from the script, leaving the exit status in $rc.
rc=0
run() {
    "$ENVC_BIN" "$@" >/dev/null 2>&1
    rc=$?
}

# ---------------------------------------------------------------------------
section "create"

run create work
check "create writes the profile file" "yes" "$([ -f "$ENVC_HOME/profiles/work/.env" ] && echo yes || echo no)"
check "create succeeds" "0" "$rc"

run create work
check "create refuses to clobber an existing profile" "1" "$rc"

run create work --force
check "create --force overwrites" "0" "$rc"

cat >"$ENVC_HOME/profiles/work/.env" <<'EOF'
# the profile used by most tests
EDITOR=vim
PROJECT=work
TOKEN="a b"
PATH="$HOME/bin:$PATH"
export EXPLICIT=yes
EOF

run create alt
cat >"$ENVC_HOME/profiles/alt/.env" <<'EOF'
EDITOR=emacs
PROJECT=alt
EOF

# Intentionally broken: used to check error reporting.
run create broken
cat >"$ENVC_HOME/profiles/broken/.env" <<'EOF'
EDITOR=ed
THIS LINE HAS NO EQUALS SIGN
EOF

# ---------------------------------------------------------------------------
section "list"

listing=$("$ENVC_BIN" list 2>/dev/null)
check_contains "list shows every profile" "work" "$listing"
check_contains "list shows every profile" "alt" "$listing"
check_contains "list reports the startup state" "startup loading" "$listing"
check_contains "list reports the selection" "selected profile" "$listing"

err=$("$ENVC_BIN" list 2>&1 >/dev/null)
check_contains "list flags the unparsable profile" "broken" "$err"

run list
check "list succeeds" "0" "$rc"

# ---------------------------------------------------------------------------
section "activate / deactivate"

out=$(bash -c '
    EDITOR=nano; export EDITOR
    unset PROJECT TOKEN EXPLICIT
    eval "$(envc activate work)"
    printf "%s|%s|%s|%s" "$EDITOR" "$PROJECT" "$TOKEN" "$EXPLICIT"
')
check "activate overrides existing values" "vim|work|a b|yes" "$out"

out=$(bash -c '
    ENVC=$(command -v envc)   # remember it before PATH is replaced
    PATH=/usr/bin:/bin
    eval "$($ENVC activate work)"
    printf "%s" "$PATH"
')
check "activate expands \$PATH from the environment" "$HOME/bin:/usr/bin:/bin" "$out"

out=$(bash -c '
    EDITOR=nano; export EDITOR
    unset PROJECT TOKEN
    eval "$(envc activate work)"
    eval "$(envc deactivate)"
    printf "%s|%s|%s" "$EDITOR" "${PROJECT-<unset>}" "${TOKEN-<unset>}"
')
check "deactivate restores replaced values" "nano|<unset>|<unset>" "$out"

out=$(bash -c '
    ENVC=$(command -v envc)
    PATH=/usr/bin
    eval "$($ENVC activate work)"
    eval "$($ENVC deactivate)"
    printf "%s" "$PATH"
')
check "deactivate restores PATH exactly" "/usr/bin" "$out"

out=$(bash -c '
    EDITOR=nano; export EDITOR
    eval "$(envc activate work)"
    eval "$(envc deactivate)"
    eval "$(envc activate alt)"
    printf "%s|%s" "$EDITOR" "$PROJECT"
')
check "a second activate works after a deactivate" "emacs|alt" "$out"

# ---------------------------------------------------------------------------
section "switching profiles rolls back to the original base"

out=$(bash -c '
    EDITOR=nano; export EDITOR
    unset PROJECT
    eval "$(envc activate work)"
    printf "mid=%s,%s " "$EDITOR" "${PROJECT-<unset>}"
    eval "$(envc activate alt)"
    printf "switched=%s,%s " "$EDITOR" "${PROJECT-<unset>}"
    eval "$(envc deactivate)"
    printf "end=%s,%s" "$EDITOR" "${PROJECT-<unset>}"
')
check "work -> alt -> deactivate lands on the original shell" \
    "mid=vim,work switched=emacs,alt end=nano,<unset>" "$out"

# ---------------------------------------------------------------------------
section "the restore stack is a diff-shaped text file"

# Run activate from a shell with a known EDITOR, then inspect what it recorded.
bash -c 'EDITOR=nano; export EDITOR; unset PROJECT; eval "$(envc activate work)"' >/dev/null

stack=$(cat "$ENVC_HOME/stack")
check_contains "stack has a '-' side" "-EDITOR=nano" "$stack"
check_contains "stack has a '+' side" "+EDITOR=vim" "$stack"
check_contains "stack records variables that were unset" "-PROJECT" "$stack"
check_contains "stack names the profile" "# active-profile work" "$stack"
check_contains "stack carries a diff header" "--- before envc" "$stack"

out=$("$ENVC_BIN" stack)
check_contains "envc stack prints the file" "-EDITOR=nano" "$out"

# ---------------------------------------------------------------------------
section "a broken profile is reported, not silently half-applied"

err=$("$ENVC_BIN" activate broken 2>&1 >/dev/null)
rc=$?
check "activate fails on a malformed line" "1" "$rc"
check_contains "the error names file and line" "broken/.env:2" "$err"

check "a failed activate leaves the stack untouched" "work" \
    "$(sed -n 's/^# active-profile //p' "$ENVC_HOME/stack")"

out=$(bash -c 'EDITOR=nano; eval "$(envc activate broken 2>/dev/null)"; printf "%s" "$EDITOR"')
check "a failed activate emits no shell code" "nano" "$out"

# ---------------------------------------------------------------------------
section "init / enable / disable"

echo 'export KEEPME=1' >"$ENVC_RC"
original=$(cat "$ENVC_RC")

run init
check "init succeeds" "0" "$rc"
check_contains "the hook is in the rc file" "# >>> envc initialize >>>" "$(cat "$ENVC_RC")"
check_contains "the hook calls autoload" "envc autoload" "$(cat "$ENVC_RC")"

run init
check "init is idempotent" "1" "$(grep -c '>>> envc initialize >>>' "$ENVC_RC")"

# A brand new shell, started from the patched rc file, picks the profile up.
out=$(bash -c "source '$ENVC_RC'; printf '%s|%s' \"\$PROJECT\" \"\$ENVC_ACTIVE\"")
check "a new shell autoloads the selected profile" "work|work" "$out"
check "the pre-existing rc content survived" "yes" \
    "$([ "$(head -1 "$ENVC_RC")" = 'export KEEPME=1' ] && echo yes || echo no)"

# The wrapper function means no manual `eval` is needed.
out=$(bash -c "source '$ENVC_RC'; envc activate alt; printf '%s|%s' \"\$EDITOR\" \"\$PROJECT\"")
check "the shell wrapper applies activate directly" "emacs|alt" "$out"

out=$(bash -c "source '$ENVC_RC'; envc de; printf '%s|%s' \"\${PROJECT-<unset>}\" \"\${ENVC_ACTIVE-<none>}\"")
check "the shell wrapper applies deactivate directly" "<unset>|<none>" "$out"

# deactivate cleared the selection, so the next shell starts clean.
out=$(bash -c "source '$ENVC_RC'; printf '%s|%s' \"\${PROJECT-<unset>}\" \"\${ENVC_ACTIVE-<none>}\"")
check "deactivate stops future shells from autoloading" "<unset>|<none>" "$out"

run disable
check "disable succeeds" "0" "$rc"
check "disable restores the rc file byte for byte" "$original" "$(cat "$ENVC_RC")"

run disable
check "disable is idempotent" "0" "$rc"

# ---------------------------------------------------------------------------
section "delete"

run activate alt
check "activate alt" "0" "$rc"

err=$("$ENVC_BIN" delete alt 2>&1 >/dev/null)
rc=$?
check "delete refuses to remove the active profile" "1" "$rc"
check_contains "delete explains why" "currently active" "$err"

run delete alt --force
check "delete --force removes the profile directory" "no" \
    "$([ -d "$ENVC_HOME/profiles/alt" ] && echo yes || echo no)"
check "delete --force clears the restore stack" "no" \
    "$([ -f "$ENVC_HOME/stack" ] && echo yes || echo no)"

run delete nonexistent
check "delete reports a missing profile" "1" "$rc"

# ---------------------------------------------------------------------------
section "bad input"

for bad in '../evil' 'a/b' '.hidden' ''; do
    run create "$bad"
    check "create rejects the name '$bad'" "1" "$rc"
done

run activate nonexistent
check "activate reports a missing profile" "1" "$rc"

run deactivate
check "deactivate with nothing active is not an error" "0" "$rc"

run status
check "status succeeds" "0" "$rc"

# ---------------------------------------------------------------------------
printf '\n%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
