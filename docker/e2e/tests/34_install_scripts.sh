#!/usr/bin/env bash
# 34_install_scripts.sh — scripts/install.sh and scripts/uninstall.sh.
#
# Runs the real scripts against the binaries this image built: the dependency
# report (complete host, and a host with QEMU hidden from PATH), a local
# install into a private bin dir, the refusal to touch systemd when no user
# instance answers (the harness has none — the assertion is that it says so and
# leaves no unit behind), and the uninstall paths: keep binaries, remove
# binaries, refuse an unattended purge without --yes, then purge a scratch data
# root with ANDLER_HOME and prove the real one was untouched.
#
# No instances are created; the scratch tree lives under E2E_WORKDIR, which the
# orchestrator removes.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Container layout: <root>/{tests,scripts}. Checkout layout: docker/e2e/tests.
SCRIPTS_DIR="$TESTS_DIR/../scripts"
if [[ ! -d "$SCRIPTS_DIR" ]]; then
    SCRIPTS_DIR="$TESTS_DIR/../../../scripts"
fi
INSTALL="$SCRIPTS_DIR/install.sh"
UNINSTALL="$SCRIPTS_DIR/uninstall.sh"

for script in "$INSTALL" "$UNINSTALL"; do
    if [[ ! -x "$script" ]]; then
        fail "install scripts are not where the suite expects them ($script)"
    fi
done
echo "  install scripts: $SCRIPTS_DIR"

TMP="$(mktemp -d "$E2E_WORKDIR/install-scripts.XXXXXX")"

# --- flags and usage --------------------------------------------------------

expect_ok "install --help runs" -- "$INSTALL" --help
expect_out_grep "--help documents the dependency report" -- "check-deps"
expect_out_grep "--help documents the release source" -- "from-release"

expect_fail "an unknown flag is rejected" -- "$INSTALL" --definitely-not-a-flag
expect_err_grep "the rejection names the flag" "unknown option"

expect_fail "an unknown component is rejected" -- "$INSTALL" --component kernel
expect_err_grep "the rejection names the accepted values" "(daemon, cli or both)"

# --- dependency report ------------------------------------------------------

expect_ok "the dependency report exits zero on a complete host" -- \
    "$INSTALL" --check-deps --component daemon --no-service
expect_out_grep "the report checks the hypervisor" "qemu-system-x86_64"
expect_out_grep "the report checks the firmware" "OVMF"
expect_out_grep "the report summarises the required set" "required dependencies"
expect_out_grep "every required dependency is present in this image" "all present"
expect_out_grep "a missing optional tool is listed with its feature" "(oras|passt)"
expect_out_grep "each gap carries an install command" "(pacman -S|apt install|dnf install|oras.land)"

# A host that cannot see QEMU must be refused, with the fix on screen — the
# check the suite is really about. PATH is rebuilt without it, so every other
# tool the script uses still resolves.
mkdir -p "$TMP/shadow"
for dir in /usr/local/bin /usr/bin /bin /usr/sbin /sbin; do
    [[ -d "$dir" ]] || continue
    for f in "$dir"/*; do
        [[ -f "$f" && -x "$f" ]] || continue
        name="$(basename "$f")"
        [[ "$name" == "qemu-system-x86_64" ]] && continue
        ln -sf "$f" "$TMP/shadow/$name"
    done
done

expect_fail "the report fails when a required dependency is missing" -- \
    env PATH="$TMP/shadow" "$INSTALL" --check-deps --component daemon --no-service
expect_out_grep "the missing dependency is named" "qemu-system-x86_64"
expect_out_grep "the report counts what is missing" "required dependencies missing"
expect_out_grep "the fix names the package" "(qemu-system-x86|qemu-kvm)"

expect_fail "an install stops when a required dependency is missing" -- \
    env PATH="$TMP/shadow" "$INSTALL" --component both --bin-dir "$TMP/bin-missing" --no-service
expect_out_grep "the refusal carries the fix command" "(pacman -S --needed|apt install|dnf install)"
expect_no_file "nothing was installed" "$TMP/bin-missing/andlerd"

# --- install ----------------------------------------------------------------

expect_ok "installs daemon + CLI into a private bin dir" -- \
    "$INSTALL" --component both --bin-dir "$TMP/bin" --no-service
expect_file "andler is installed" "$TMP/bin/andler"
expect_file "andlerd is installed" "$TMP/bin/andlerd"

expect_ok "the installed CLI runs" -- "$TMP/bin/andler" --version
expect_out_grep "the installed CLI reports a version" "andler [0-9]+\.[0-9]+"
expect_ok "the installed daemon runs" -- "$TMP/bin/andlerd" --version
expect_out_grep "the installed daemon reports a version" "andlerd [0-9]+\.[0-9]+"

expect_ok "re-running the installer is idempotent" -- \
    "$INSTALL" --component both --bin-dir "$TMP/bin" --no-service
expect_file "andler is still there" "$TMP/bin/andler"
expect_file "andlerd is still there" "$TMP/bin/andlerd"

# ...and its own version: the CLI and the daemon the suite just installed must
# agree, or the pair is unusable (the CLI refuses a daemon from another build).
cli_version="$("$TMP/bin/andler" --version | awk '{print $NF}')"
daemon_version="$("$TMP/bin/andlerd" --version | awk '{print $NF}')"
if [[ "$cli_version" == "$daemon_version" ]]; then
    pass "the installed pair reports one version ($cli_version)"
else
    fail "the installed pair drifted: andler $cli_version vs andlerd $daemon_version"
fi

# --- systemd ----------------------------------------------------------------

# The dependency report has to say what it thinks of the user manager; the
# assertions differ by host because the side effect would too. The containerized
# harness has no systemd user instance, so requesting the service must refuse
# with the reason and the way out, before anything is written. A developer host
# usually has one, and installing there would write into the real
# ~/.config/systemd/user — so the assertion is the opposite: the report counts
# the user manager as satisfied, and installs nothing.
if command -v systemctl >/dev/null 2>&1 && timeout 5 systemctl --user show-environment >/dev/null 2>&1; then
    echo "  (systemd user instance present — asserting the satisfied branch)"
    expect_ok "a live systemd user instance satisfies the service dependency" -- \
        "$INSTALL" --check-deps --component daemon
    expect_out_grep "the report counts systemd as satisfied" "systemd --user"
    expect_out_grep "no required dependency is missing" "all present"
    expect_no_file "--check-deps installed nothing" "$TMP/bin-service/andlerd"
else
    expect_fail "the service install refuses when no systemd user instance answers" -- \
        "$INSTALL" --component daemon --bin-dir "$TMP/bin-service"
    expect_out_grep "the refusal names systemd" "systemd"
    expect_out_grep "the refusal names the way out" "no-service"
    expect_no_file "no unit file was written" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/andlerd.service"

    # The same host, with the dependency gate bypassed: the service step itself
    # has to refuse, after the binaries are in place.
    expect_fail "--skip-deps reaches the service step and refuses there too" -- \
        "$INSTALL" --component daemon --bin-dir "$TMP/bin-service" --skip-deps
    expect_out_grep "the service step names systemd" "systemd"
    expect_file "the binaries were installed before the refusal" "$TMP/bin-service/andlerd"
    expect_no_file "still no unit file" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/andlerd.service"
fi

# --- uninstall --------------------------------------------------------------

expect_ok "uninstall without --binaries leaves the binaries alone" -- "$UNINSTALL"
expect_file "andler survived" "$TMP/bin/andler"
expect_file "andlerd survived" "$TMP/bin/andlerd"
expect_out_grep "the data root is reported as kept" "left in place"

mkdir -p "$TMP/home/instances" "$TMP/home/cache"
echo "test" >"$TMP/home/andlerd.db"

expect_fail "a purge without a TTY refuses" -- \
    env ANDLER_HOME="$TMP/home" sh -c 'exec "$1" --purge </dev/null' sh "$UNINSTALL"
expect_err_grep "the refusal names --yes" -- "--yes"
expect_file "the scratch data root survived the refusal" "$TMP/home/andlerd.db"

expect_ok "an unattended purge deletes the data root ANDLER_HOME points at" -- \
    env ANDLER_HOME="$TMP/home" "$UNINSTALL" --purge --yes
expect_no_file "the scratch data root is gone" "$TMP/home"
expect_no_file "the scratch data root is really gone (no stray parent)" "$TMP/home/instances"

# The purge follows ANDLER_HOME and nothing else: the root the harness itself
# uses has to be exactly as it was.
real_root="${ANDLER_HOME:-$HOME/.andler}"
if [[ -e "$real_root" ]]; then
    expect_file "the harness's own data root was not touched" "$real_root"
else
    pass "no separate data root existed to protect ($real_root)"
fi

expect_ok "uninstall --binaries removes what install.sh put there" -- \
    "$UNINSTALL" --binaries --bin-dir "$TMP/bin"
expect_no_file "andler is gone" "$TMP/bin/andler"
expect_no_file "andlerd is gone" "$TMP/bin/andlerd"

expect_ok "uninstall --binaries on an empty dir is not an error" -- \
    "$UNINSTALL" --binaries --bin-dir "$TMP/bin"

rm -rf "$TMP"
