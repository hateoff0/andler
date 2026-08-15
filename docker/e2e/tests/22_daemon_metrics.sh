#!/usr/bin/env bash
# andler doctor --metrics: snapshot shape and live RPC counters.
set -u
E2E_ROOT="${E2E_ROOT:-/usr/local/share/andler-e2e}"
# shellcheck source=common.sh
source "$E2E_ROOT/tests/common.sh"

WORK="$E2E_WORKDIR/22-daemon-metrics"
mkdir -p "$WORK"

echo "  [snapshot shape]"
expect_ok "doctor --metrics prints the snapshot" -- timeout 10 andler doctor --metrics
expect_out_grep "header present" "daemon metrics"
expect_out_grep "counts line" "instances: 0 total"

echo "  [live RPC traffic is recorded]"
expect_ok "a normal command still works" -- andler list
expect_ok "metrics show recorded RPC traffic" -- timeout 10 andler doctor --metrics
expect_out_grep "GetVersion recorded" "GetVersion"
expect_out_grep "ListInstances recorded" "ListInstances"

echo "  [cleanup]"
expect_ok "list empty" -- andler list
expect_out_grep "no instances" "no instances"
