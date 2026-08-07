#!/usr/bin/env bash
# Functional tests for fetch-waydroid-images.py against a local fixture server:
# fresh download, cache reuse, Range resume, and the MD5-mismatch fatal path.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FETCH="$SCRIPT_DIR/../base/rootfs/usr/local/lib/andler/fetch-waydroid-images.py"
SERVER="$SCRIPT_DIR/fixture_server.py"

PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); echo "  ok - $1"; }
fail() { FAIL=$((FAIL + 1)); echo "  FAIL - $1"; }

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$WORK/site" "$WORK/cache" "$WORK/logs"

PORT="$(python3 - <<'EOF'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
EOF
)"

# --- Fixture site: two small zips (real zip structure, known MD5) -------------

python3 - "$WORK/site" "$PORT" <<'EOF'
import hashlib, os, random, sys, zipfile
site, port = sys.argv[1], int(sys.argv[2])

def make_zip(name, payload_bytes, member):
    path = os.path.join(site, name)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(member, random.Random(42).randbytes(payload_bytes))
    digest = hashlib.md5(open(path, "rb").read()).hexdigest()
    return digest, os.path.getsize(path)

system_name = "lineage-20.0-20260101-VANILLA-waydroid_x86_64-system.zip"
vendor_name = "lineage-20.0-20260101-MAINLINE-waydroid_x86_64-vendor.zip"
system_md5, system_size = make_zip(system_name, 3 * 1024 * 1024 + 777, "system.img")
vendor_md5, vendor_size = make_zip(vendor_name, 900 * 1024 + 123, "vendor.img")

def item(name, size, md5):
    return (
        "<item>\n"
        f"  <title>/images/system/lineage/waydroid_x86_64/{name}</title>\n"
        f"  <link>http://127.0.0.1:{port}/{name}</link>\n"
        f'  <media:content filesize="{size}"/>\n'
        f'  <media:hash algo="md5">{md5}</media:hash>\n'
        "</item>"
    )

rss = (
    '<?xml version="1.0"?>\n'
    '<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/">\n'
    "<channel>\n"
    f"{item(system_name, system_size, system_md5)}\n"
    f"{item(vendor_name, vendor_size, vendor_md5)}\n"
    "</channel>\n"
    "</rss>\n"
)
with open(os.path.join(site, "rss"), "w") as f:
    f.write(rss)
EOF

python3 "$SERVER" --directory "$WORK/site" --port "$PORT" >"$WORK/logs/server.log" 2>&1 &
SERVER_PID=$!
sleep 0.3

RSS_BASE="http://127.0.0.1:${PORT}/rss?path="
CACHE="$WORK/cache"
SYSTEM_ZIP="$CACHE/lineage-20.0-20260101VANILLA-waydroid_x86_64-system.zip"
export ANDLER_FETCH_PROGRESS_INTERVAL=0

run_fetch() { # out_dir [extra args...]
    local out="$1"
    shift
    python3 "$FETCH" --android-major 13 --variant VANILLA \
        --out "$out" --rss-base "$RSS_BASE" "$@"
}

# --- 1. Fresh download (no cache dir) ----------------------------------------

echo "==> 1. fresh download"
mkdir -p "$WORK/out1"
if run_fetch "$WORK/out1" >"$WORK/logs/run1.out" 2>"$WORK/logs/run1.err"; then ok "fresh run exits 0"; else fail "fresh run exits 0"; fi
[[ -f "$WORK/out1/system.img" ]] && ok "system.img extracted" || fail "system.img extracted"
[[ -f "$WORK/out1/vendor.img" ]] && ok "vendor.img extracted" || fail "vendor.img extracted"
[[ -f "$WORK/out1/andler-fetch-manifest.json" ]] && ok "manifest written" || fail "manifest written"
grep -q '"system_md5_verified": true' "$WORK/out1/andler-fetch-manifest.json" && ok "manifest md5_verified" || fail "manifest md5_verified"
grep -q "MD5 OK" "$WORK/logs/run1.out" && ok "md5 verified in output" || fail "md5 verified in output"
grep -qE "\(100\.0%\)" "$WORK/logs/run1.out" && ok "progress line printed" || fail "progress line printed"
grep -qE "GET /lineage-20\.0-20260101-VANILLA-waydroid_x86_64-system\.zip/download 200" "$WORK/logs/server.log" \
    && ok "server received system zip request" || fail "server received system zip request"

# --- 2. First run with a persistent cache dir (downloads there, keeps them) ---

echo "==> 2: download into cache dir"
mkdir -p "$WORK/out2"
if run_fetch "$WORK/out2" --cache-dir "$CACHE" >"$WORK/logs/run2.out" 2>"$WORK/logs/run2.err"; then ok "cached run exits 0"; else fail "cached run exits 0"; fi
[[ -f "$CACHE/lineage-20.0-20260101-VANILLA-waydroid_x86_64-system.zip" ]] \
    && ok "system zip kept in cache" || fail "system zip kept in cache"
cache_reqs_after_2="$(grep -c "GET /lineage" "$WORK/logs/server.log")"

# --- 3. Cache reuse: no new requests for either zip --------------------------

echo "==> 3: cache reuse"
mkdir -p "$WORK/out3"
if run_fetch "$WORK/out3" --cache-dir "$CACHE" >"$WORK/logs/run3.out" 2>"$WORK/logs/run3.err"; then ok "reuse run exits 0"; else fail "reuse run exits 0"; fi
grep -q "already in cache, MD5 OK" "$WORK/logs/run3.out" && ok "cache hit reported" || fail "cache hit reported"
cache_reqs_after_3="$(grep -c "GET /lineage" "$WORK/logs/server.log")"
if [[ "$cache_reqs_after_3" -eq "$cache_reqs_after_2" ]]; then ok "no re-download on cache hit ($cache_reqs_after_3)"; else fail "no re-download on cache hit ($cache_reqs_after_3 vs $cache_reqs_after_2)"; fi

# --- 4. Range resume from a partial cached file -------------------------------

echo "==> 4: resume truncated cached zip"
truncate -s 100000 "$CACHE/lineage-20.0-20260101-VANILLA-waydroid_x86_64-system.zip"
mkdir -p "$WORK/out4"
if run_fetch "$WORK/out4" --cache-dir "$CACHE" >"$WORK/logs/run4.out" 2>"$WORK/logs/run4.err"; then ok "resume run exits 0"; else fail "resume run exits 0"; fi
grep -qE "GET /lineage-20\.0-20260101-VANILLA-waydroid_x86_64-system\.zip/download .*range=bytes=100000-" \
    "$WORK/logs/server.log" && ok "resumed via Range (206, bytes=100000-)" || fail "resumed via Range"
grep -q "MD5 OK" "$WORK/logs/run4.out" && ok "resumed file verifies md5" || fail "resumed file verifies md5"

# --- 5. MD5 mismatch on a poisoned upstream file ------------------------------

echo "==> 5: md5 mismatch fatal path"
rm -f "$CACHE"/*.zip
SIZE="$(stat -c%s "$WORK/site/lineage-20.0-20260101-VANILLA-waydroid_x86_64-system.zip")"
head -c "$SIZE" /dev/zero > "$WORK/site/lineage-20.0-20260101-VANILLA-waydroid_x86_64-system.zip"
mkdir -p "$WORK/out5"
if run_fetch "$WORK/out5" --cache-dir "$CACHE" >"$WORK/logs/run5.out" 2>"$WORK/logs/run5.err"; then
    fail "corrupt upstream must fail the build"
else
    ok "corrupt upstream fails the build"
    grep -q "MD5 mismatch" "$WORK/logs/run5.out" "$WORK/logs/run5.err" 2>/dev/null \
        && ok "MD5 mismatch reported" || fail "MD5 mismatch reported"
fi

echo
echo "passed: $PASS, failed: $FAIL"
[[ "$FAIL" -eq 0 ]]