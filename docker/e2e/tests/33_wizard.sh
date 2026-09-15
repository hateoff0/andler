#!/usr/bin/env bash
# 33 — the interactive wizard over a real PTY. `script` provides the terminal
# cliclack insists on and forwards the key sequence into it. The keys are sent
# only once the first prompt is on screen: cliclack reads keys in raw mode,
# and a key that arrives while the terminal is still canonical can sit in the
# line buffer forever (an unterminated Esc never leaves it). What is asserted
# afterwards is the daemon-side result of the answers — the created instance's
# config — not the rendering; the session text is checked for its markers.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/33-wizard"
mkdir -p "$WORK"

# Instance home the daemon writes to: ANDLER_HOME when the harness set one,
# otherwise the default every suite's fixtures already assume.
INSTANCES_ROOT="${ANDLER_HOME:-$HOME/.andler}/instances"

# drive_wizard <session-file> <keys> [extra create args...] — run the
# interactive wizard on a pty, feeding the keys once the first prompt shows.
# Every prompt opens on its default answer, so a sequence only carries what it
# changes: recommended mode, Linux, a name, no ISO, 512 GiB, legacy BIOS.
drive_wizard() {
    local session="$1" keys="$2"
    shift 2
    : >"$session"
    (
        for _ in $(seq 1 200); do
            if grep -q "How much do you want to configure" "$session" 2>/dev/null; then
                break
            fi
            sleep 0.05
        done
        sleep 0.2
        printf '%s' "$keys"
    ) | script -qec \
        "${ANDLER_BIN:-/usr/local/bin/andler} --daemon-addr http://$E2E_LISTEN_ADDR create $*" \
        /dev/null >"$session" 2>&1
}

instance_id_of() {
    andler list --json | jq -r --arg name "$1" '.[] | select(.name == $name) | .id'
}

# session_grep/session_nogrep <description> <pattern> <session-file> — the
# wizard's own output, kept out of $E2E_LAST_OUT so a later command cannot
# overwrite what is being asserted.
session_grep() {
    if grep -qE -- "$2" "$3"; then
        pass "$1"
    else
        sed 's/^/      session: /' "$3" | head -30
        fail "$1 (pattern '$2' not found in the session output)"
    fi
}

session_nogrep() {
    if grep -qE -- "$2" "$3"; then
        sed 's/^/      session: /' "$3" | head -30
        fail "$1 (pattern '$2' unexpectedly found)"
    else
        pass "$1"
    fi
}

echo "  [the wizard creates the VM its answers describe]"
SESSION="$WORK/create.session"
if drive_wizard "$SESSION" $'\r\rwizard-e2e\r\r512\rn\r'; then
    pass "wizard exits zero after creating"
else
    sed 's/^/      session: /' "$SESSION" | head -30
    fail "wizard exited non-zero"
fi
session_grep "the answers are reviewed before creating" "review" "$SESSION"
session_grep "the declined UEFI answer is on the summary" "Legacy BIOS" "$SESSION"
session_grep "the session closes with its outro" "The instance is ready" "$SESSION"
session_nogrep "the interactive flow ran, not the scripted refusal" "no TTY" "$SESSION"

ID="$(instance_id_of wizard-e2e)"
[[ -n "$ID" ]] || fail "the wizard created an instance named wizard-e2e"

expect_ok "the created instance resolves" -- andler config view "$ID"
expect_out_grep "the typed disk size reached the config" "size_bytes: 512 GiB"
expect_file "the instance home holds its config" "$INSTANCES_ROOT/$ID/instance.toml"
if grep -qE '^enable_uefi = false$' "$INSTANCES_ROOT/$ID/instance.toml"; then
    pass "the declined UEFI answer reached instance.toml"
else
    fail "enable_uefi = false is not in $INSTANCES_ROOT/$ID/instance.toml"
fi

echo "  [Esc cancels without creating anything]"
BEFORE="$(andler list --json | jq 'length')"
CANCEL="$WORK/cancel.session"
if drive_wizard "$CANCEL" $'\033'; then
    pass "a cancelled wizard exits zero"
else
    fail "a cancelled wizard exited non-zero"
fi
session_grep "the cancellation says nothing was created" "Nothing was created" "$CANCEL"
session_nogrep "no report panel after a cancel" "next steps" "$CANCEL"
AFTER="$(andler list --json | jq 'length')"
if [[ "$BEFORE" == "$AFTER" ]]; then
    pass "a cancelled wizard creates no instance"
else
    fail "instance count changed on cancel: $BEFORE -> $AFTER"
fi

andler remove --purge "$ID" >/dev/null 2>&1 || fail "cleanup: could not remove $ID"
if [[ -z "$(instance_id_of wizard-e2e)" ]]; then
    pass "cleanup removed the wizard's instance"
else
    fail "the wizard's instance outlived the suite"
fi
