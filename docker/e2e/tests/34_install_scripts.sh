#!/usr/bin/env bash
# 34_install_scripts.sh — scripts/install.sh and scripts/uninstall.sh.
#
# Runs the real scripts: the dependency report (complete host, and a host with
# QEMU hidden from PATH), the flag contract (--check-deps, --skip-deps,
# --with-optional, --dry-run and the combinations it refuses), a local install
# into a scratch bin dir with the two versions compared, the `andler doctor`
# pass the installer runs when a daemon answers here (the harness daemon does),
# the systemd branch appropriate to the host, and the uninstall paths: keep
# binaries, refuse an unattended purge or package removal without --yes, purge a
# scratch ANDLER_HOME with the harness's own data root untouched, then remove
# the binaries.
#
# The scripts refuse to install as root — they write a per-user service and
# per-user data. The containerized harness *is* root, so the install assertions
# run as a scratch user created here (`runuser`), while the read-only modes
# (--check-deps, --dry-run) run in the current shell, which is itself part of
# the contract. No instances are created; the scratch tree lives under
# E2E_WORKDIR or the scratch user's home, both of which the orchestrator drops.

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

IS_ROOT=0
[[ "${EUID:-$(id -u)}" -eq 0 ]] && IS_ROOT=1

# Who the install assertions run as: the current user when it is not root, a
# scratch user otherwise.
SUITE_USER=""
SUITE_HOME=""
if (( IS_ROOT )); then
    if command -v useradd >/dev/null 2>&1 && command -v runuser >/dev/null 2>&1; then
        SUITE_USER="andler-e2e"
        if ! id "$SUITE_USER" >/dev/null 2>&1; then
            useradd -m "$SUITE_USER" >/dev/null 2>&1 || SUITE_USER=""
        fi
        if [[ -n "$SUITE_USER" ]]; then
            SUITE_HOME="$(getent passwd "$SUITE_USER" | cut -d: -f6)"
        fi
    fi
fi

as_install_user() {
    if [[ -n "$SUITE_USER" ]]; then
        runuser -u "$SUITE_USER" -- env HOME="$SUITE_HOME" "$@"
    else
        "$@"
    fi
}

if [[ -n "$SUITE_USER" ]]; then
    echo "  installing as scratch user: $SUITE_USER ($SUITE_HOME)"
fi

WORK="$E2E_WORKDIR/install-scripts"
mkdir -p "$WORK"
if [[ -n "$SUITE_USER" ]]; then
    WORK="$SUITE_HOME/e2e-install-scripts"
    as_install_user mkdir -p "$WORK"
fi

# --- flags and usage --------------------------------------------------------

expect_ok "install --help runs" -- "$INSTALL" --help
expect_out_grep "--help documents the dependency report" -- "check-deps"
expect_out_grep "--help documents the release source" -- "from-release"
expect_out_grep "--help documents the optional set" -- "with-optional"
expect_out_grep "--help documents the dry run" -- "dry-run"

expect_fail "an unknown flag is rejected" -- "$INSTALL" --definitely-not-a-flag
expect_err_grep "the rejection names the flag" "unknown option"

expect_fail "an unknown component is rejected" -- "$INSTALL" --component kernel
expect_err_grep "the rejection names the accepted values" "(daemon, cli or both)"

expect_fail "--check-deps and --skip-deps are refused together" -- "$INSTALL" --check-deps --skip-deps
expect_err_grep "the contradiction is named" "contradict"

expect_fail "--with-optional without a check is refused" -- "$INSTALL" --skip-deps --with-optional
expect_err_grep "the refusal explains why" "dependency check"

expect_fail "--check-deps with --with-optional is refused" -- "$INSTALL" --check-deps --with-optional
expect_err_grep "the refusal explains why" "installs no packages"

if (( IS_ROOT )); then
    # The read-only modes are the ones a container or CI image needs.
    expect_ok "--check-deps is allowed as root" -- "$INSTALL" --check-deps --component daemon --no-service
    expect_fail "installing as root is refused" -- "$INSTALL" --component daemon --no-service --bin-dir "$WORK/bin-root"
    expect_err_grep "the refusal explains what to do" "normal user"
fi

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
mkdir -p "$WORK/shadow"
for dir in /usr/local/bin /usr/bin /bin /usr/sbin /sbin; do
    [[ -d "$dir" ]] || continue
    for f in "$dir"/*; do
        [[ -f "$f" && -x "$f" ]] || continue
        name="$(basename "$f")"
        [[ "$name" == "qemu-system-x86_64" ]] && continue
        ln -sf "$f" "$WORK/shadow/$name"
    done
done

expect_fail "the report fails when a required dependency is missing" -- \
    env PATH="$WORK/shadow" "$INSTALL" --check-deps --component daemon --no-service
expect_out_grep "the missing dependency is named" "qemu-system-x86_64"
expect_out_grep "the report counts what is missing" "required dependencies missing"
expect_out_grep "the fix names the package" "(qemu-system-x86|qemu-kvm)"

expect_fail "an install stops when a required dependency is missing" -- \
    env PATH="$WORK/shadow" "$INSTALL" --component both --bin-dir "$WORK/bin-missing" --no-service
expect_out_grep "the refusal carries the fix command" "(pacman -S --needed|apt install|dnf install)"
expect_no_file "nothing was installed" "$WORK/bin-missing/andlerd"

# --- the optional set -------------------------------------------------------

# --dry-run prints the package command and changes nothing, so it is safe to
# assert here whatever the host has installed: `oras` and the NVIDIA driver are
# never part of the set, and the rest may or may not be missing.
expect_ok "--dry-run --with-optional prints the plan" -- \
    "$INSTALL" --dry-run --with-optional --component daemon --no-service --bin-dir "$WORK/bin-dry"
expect_out_grep "--dry-run names the package manager" "(pacman -S --needed --noconfirm|apt install -y|dnf install -y)"
expect_out_grep "--dry-run says nothing happened" "nothing was downloaded, installed or written"
expect_no_file "--dry-run installed no binaries" "$WORK/bin-dry/andlerd"

# --- install ----------------------------------------------------------------

if (( IS_ROOT )) && [[ -z "$SUITE_USER" ]]; then
    echo "  SKIP: root without useradd/runuser — the install paths cannot be driven here"
    rm -rf "$WORK"
    exit 0
fi

expect_ok "installs daemon + CLI into a private bin dir" -- \
    as_install_user "$INSTALL" --component both --bin-dir "$WORK/bin" --no-service
expect_file "andler is installed" "$WORK/bin/andler"
expect_file "andlerd is installed" "$WORK/bin/andlerd"

expect_ok "the installed CLI runs" -- "$WORK/bin/andler" --version
expect_out_grep "the installed CLI reports a version" "andler [0-9]+\.[0-9]+"
expect_ok "the installed daemon runs" -- "$WORK/bin/andlerd" --version
expect_out_grep "the installed daemon reports a version" "andlerd [0-9]+\.[0-9]+"

expect_ok "re-running the installer is idempotent" -- \
    as_install_user "$INSTALL" --component both --bin-dir "$WORK/bin" --no-service
expect_file "andler is still there" "$WORK/bin/andler"
expect_file "andlerd is still there" "$WORK/bin/andlerd"

# ...and its own version: the CLI and the daemon the suite just installed must
# agree, or the pair is unusable (the CLI refuses a daemon from another build).
cli_version="$("$WORK/bin/andler" --version | awk '{print $NF}')"
daemon_version="$("$WORK/bin/andlerd" --version | awk '{print $NF}')"
if [[ "$cli_version" == "$daemon_version" ]]; then
    pass "the installed pair reports one version ($cli_version)"
else
    fail "the installed pair drifted: andler $cli_version vs andlerd $daemon_version"
fi

# --- the doctor pass --------------------------------------------------------

# The installer ends with `andler doctor` when a daemon answers on this host;
# the harness daemon does, on the address the orchestrator exports.
expect_ok "an install against a live local daemon ends with doctor" -- \
    as_install_user env ANDLERD_LISTEN_ADDR="${E2E_LISTEN_ADDR:?}" \
    "$INSTALL" --component cli --bin-dir "$WORK/bin" --no-service
expect_out_grep "the doctor pass ran" "andler doctor"
expect_out_grep "doctor reached the daemon" "andlerd: reachable at"
expect_out_grep "the doctor pass is summarised" "(host, daemon and base images check out|found something to look at)"
expect_out_grep "the run still reports success where it did" "Installed"

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
    expect_no_file "--check-deps installed nothing" "$WORK/bin-service/andlerd"
else
    expect_fail "the service install refuses when no systemd user instance answers" -- \
        as_install_user "$INSTALL" --component daemon --bin-dir "$WORK/bin-service"
    expect_out_grep "the refusal names systemd" "systemd"
    expect_out_grep "the refusal names the way out" "no-service"
    expect_no_file "no unit file was written" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/andlerd.service"

    # The same host, with the dependency gate bypassed: the service step itself
    # has to refuse, after the binaries are in place.
    expect_fail "--skip-deps reaches the service step and refuses there too" -- \
        as_install_user "$INSTALL" --component daemon --bin-dir "$WORK/bin-service" --skip-deps
    expect_out_grep "the service step names systemd" "systemd"
    expect_file "the binaries were installed before the refusal" "$WORK/bin-service/andlerd"
    expect_no_file "still no unit file" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/andlerd.service"
fi

# --- uninstall --------------------------------------------------------------

expect_ok "uninstall without --binaries leaves the binaries alone" -- as_install_user "$UNINSTALL"
expect_file "andler survived" "$WORK/bin/andler"
expect_file "andlerd survived" "$WORK/bin/andlerd"
expect_out_grep "the data root is reported as kept" "left in place"

expect_ok "uninstall --optional without a record is not an error" -- \
    as_install_user env ANDLER_HOME="$WORK/home-empty" "$UNINSTALL" --optional --bin-dir "$WORK/bin"
expect_out_grep "the missing record is explained" "no record at"

mkdir -p "$WORK/home/instances" "$WORK/home/cache"
echo "test" >"$WORK/home/andlerd.db"
printf 'passt\n' >"$WORK/home/optional-deps.txt"

expect_ok "the recorded optional set is reported as left installed" -- \
    as_install_user env ANDLER_HOME="$WORK/home" "$UNINSTALL" --bin-dir "$WORK/bin"
expect_out_grep "the recorded packages are named" "passt"
expect_out_grep "the removal flag is offered" -- "--optional"

# Removing system packages is destructive: it needs the same typed confirmation
# as a purge, and a non-TTY run must refuse it *before* touching the manager.
expect_fail "an unattended package removal is refused" -- \
    as_install_user sh -c 'exec env ANDLER_HOME="$1" "$2" --optional --bin-dir "$3" </dev/null' sh \
    "$WORK/home" "$UNINSTALL" "$WORK/bin"
expect_err_grep "the refusal names --yes" -- "--yes"
expect_file "the record survived the refusal" "$WORK/home/optional-deps.txt"

expect_fail "a purge without a TTY refuses" -- \
    as_install_user sh -c 'exec env ANDLER_HOME="$1" "$2" --purge </dev/null' sh "$WORK/home" "$UNINSTALL"
expect_err_grep "the refusal names --yes" -- "--yes"
expect_file "the scratch data root survived the refusal" "$WORK/home/andlerd.db"

expect_ok "an unattended purge deletes the data root ANDLER_HOME points at" -- \
    as_install_user env ANDLER_HOME="$WORK/home" "$UNINSTALL" --purge --yes
expect_no_file "the scratch data root is gone" "$WORK/home"
expect_no_file "the scratch data root is really gone (no stray parent)" "$WORK/home/instances"

# The purge follows ANDLER_HOME and nothing else: the root the harness itself
# uses has to be exactly as it was.
real_root="${ANDLER_HOME:-$HOME/.andler}"
if [[ -e "$real_root" ]]; then
    expect_file "the harness's own data root was not touched" "$real_root"
else
    pass "no separate data root existed to protect ($real_root)"
fi

expect_ok "uninstall --binaries removes what install.sh put there" -- \
    as_install_user "$UNINSTALL" --binaries --bin-dir "$WORK/bin"
expect_no_file "andler is gone" "$WORK/bin/andler"
expect_no_file "andlerd is gone" "$WORK/bin/andlerd"

expect_ok "uninstall --binaries on an empty dir is not an error" -- \
    as_install_user "$UNINSTALL" --binaries --bin-dir "$WORK/bin"

rm -rf "$WORK"
