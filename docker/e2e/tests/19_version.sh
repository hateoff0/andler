#!/usr/bin/env bash
# 19 — version handshake: the CLI queries the daemon version up front and
# runs only when both sides were built together.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

WORK="$E2E_WORKDIR/19-version"
mkdir -p "$WORK"

echo "  [matching versions pass through]"
ID="$(create_linux "$WORK/li" e2e-version)"
[[ -n "$ID" ]] || fail "empty id from create"
expect_ok "status works with a matching daemon" -- andler status "$ID"
expect_out_grep "state shown" "Created"

echo "  [cleanup]"
expect_ok "remove --purge" -- andler remove "$ID" --purge
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
