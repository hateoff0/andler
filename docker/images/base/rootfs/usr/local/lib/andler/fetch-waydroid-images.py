#!/usr/bin/env python3
"""Download system.img/vendor.img for the requested Android version from SourceForge."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from concurrent.futures import ThreadPoolExecutor

SF_PROJECT = "waydroid"
SF_RSS_URL = f"https://sourceforge.net/projects/{SF_PROJECT}/rss?path="

# lineage-18.1.x == Android 11, lineage-20.0.x == Android 13
LINEAGE_VERSION = {
    "11": "18.1",
    "13": "20.0",
}

USER_AGENT = "andler-image-builder/1 (+https://github.com/andler-project/andler)"

RSS_ATTEMPTS = 4
RSS_TIMEOUT_S = 30
DOWNLOAD_ATTEMPTS = 4
STREAM_TIMEOUT_S = 60
STALL_TIMEOUT_S = 120
FILE_DEADLINE_S = 90 * 60
PROGRESS_INTERVAL_S = float(os.environ.get("ANDLER_FETCH_PROGRESS_INTERVAL", "10"))
CHUNK = 1024 * 1024


class FetchError(Exception):
    """Failure that ends the build once retries are exhausted."""


def log(msg: str) -> None:
    print(msg, flush=True)


def format_bytes(n: int) -> str:
    for unit, divisor in (("GiB", 1024**3), ("MiB", 1024**2), ("KiB", 1024)):
        if n >= divisor:
            return f"{n / divisor:.1f} {unit}"
    return f"{n} B"


def format_duration(secs: float) -> str:
    secs = int(secs)
    if secs >= 3600:
        return f"{secs // 3600}h{(secs % 3600) // 60}m"
    if secs >= 60:
        return f"{secs // 60}m{secs % 60:02d}s"
    return f"{secs}s"


def sf_rss(path: str, base: str = SF_RSS_URL, attempts: int = RSS_ATTEMPTS) -> ET.Element:
    # Retry: single network failure on small RSS request breaks entire build
    url = base + path
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    last_error: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(req, timeout=RSS_TIMEOUT_S) as resp:
                data = resp.read()
            return ET.fromstring(data)
        except (urllib.error.URLError, TimeoutError, ConnectionError, ET.ParseError) as exc:
            last_error = exc
            if attempt < attempts:
                delay = 5 * attempt
                log(f"    RSS feed {path}: failed ({exc}), retrying in {delay}s...")
                time.sleep(delay)
    raise FetchError(f"fetch-waydroid-images: failed to fetch RSS feed {path}: {last_error}")


def latest_matching(root: ET.Element, pattern: str) -> tuple[str | None, str | None, str | None, int | None]:
    """First file in the parsed RSS feed whose name matches pattern."""
    rx = re.compile(pattern)
    for item in root.iter("item"):
        title = (item.findtext("title") or "").strip()
        name = title.rsplit("/", 1)[-1]
        if rx.search(name):
            link = item.findtext("link") or ""
            if link and not link.rstrip("/").endswith("download"):
                link = link.rstrip("/") + "/download"

            md5 = None
            size_bytes = None
            for elem in item.iter():
                if elem.tag.endswith("}content"):
                    raw_size = elem.get("filesize")
                    if raw_size and raw_size.isdigit():
                        size_bytes = int(raw_size)
                if elem.tag.endswith("}hash") and elem.get("algo") == "md5":
                    md5 = (elem.text or "").strip().lower() or None

            return name, link, md5, size_bytes
    return None, None, None, None


def md5_of(path: str) -> str:
    hasher = hashlib.md5()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            hasher.update(chunk)
    return hasher.hexdigest()


def check_disk_space(path: str, required_bytes: int, label: str) -> None:
    """Fail before download if disk space is insufficient."""
    check_path = path
    while not os.path.exists(check_path):
        parent = os.path.dirname(check_path)
        if parent == check_path:
            break
        check_path = parent
    free = shutil.disk_usage(check_path).free
    needed = int(required_bytes * 1.15)
    if free < needed:
        raise FetchError(
            f"fetch-waydroid-images: insufficient space for {label} "
            f"on {check_path} — need ~{format_bytes(needed)}, have {format_bytes(free)}"
        )


def check_unpack_space(out_dir: str, zips: list[str]) -> None:
    """Check the real uncompressed size of the zips against the extraction dir."""
    total = 0
    for zip_path in zips:
        with zipfile.ZipFile(zip_path) as zf:
            total += sum(info.file_size for info in zf.infolist())
    free = shutil.disk_usage(out_dir).free
    if free < total:
        raise FetchError(
            f"fetch-waydroid-images: insufficient space to unpack in {out_dir} "
            f"— need ~{format_bytes(total)}, have {format_bytes(free)}"
        )


class Progress:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._last = 0.0

    def report(self, label: str, done: int, total: int | None, started: float) -> None:
        now = time.monotonic()
        with self._lock:
            if now - self._last < PROGRESS_INTERVAL_S:
                return
            self._last = now
        self.line(label, done, total, started)

    def finish(self, label: str, done: int, total: int | None, started: float) -> None:
        with self._lock:
            self._last = time.monotonic()
        self.line(label, done, total, started)

    def line(self, label: str, done: int, total: int | None, started: float) -> None:
        elapsed = time.monotonic() - started
        speed = done / elapsed if elapsed > 1 else 0.0
        pct = f"{(done / total) * 100:.1f}%" if total else "?"
        eta = ""
        if total and speed > 0 and done < total:
            eta = f" ETA {format_duration((total - done) / speed)}"
        log(f"    {label}: {format_bytes(done)}/{format_bytes(total) if total else '?'} ({pct}) "
            f"{format_bytes(int(speed))}/s{eta}")


PROGRESS = Progress()


def looks_like_html(chunk: bytes) -> bool:
    head = chunk[:512].lstrip().lower()
    return head.startswith(b"<!doctype html") or head.startswith(b"<html")


def download_file(
    url: str,
    dest: str,
    label: str,
    expected_size: int | None,
    resume_allowed: bool = True,
) -> None:
    """Download url to dest with resume, progress, stall detection and retries."""
    attempt = 0
    while True:
        attempt += 1
        try:
            download_once(url, dest, label, expected_size, resume_allowed)
            actual = os.path.getsize(dest)
            if expected_size is not None and actual != expected_size:
                raise FetchError(
                    f"size mismatch (expected {format_bytes(expected_size)}, got {format_bytes(actual)})"
                )
            return
        except FetchError as exc:
            if attempt >= DOWNLOAD_ATTEMPTS:
                raise FetchError(f"fetch-waydroid-images: failed to download {label}: {exc}") from exc
            partial = os.path.getsize(dest) if os.path.exists(dest) else 0
            delay = 10 * attempt
            log(f"    {label}: {exc} — retry {attempt}/{DOWNLOAD_ATTEMPTS} in {delay}s "
                f"(resuming from {format_bytes(partial)})")
            time.sleep(delay)


def download_once(
    url: str,
    dest: str,
    label: str,
    expected_size: int | None,
    resume_allowed: bool,
) -> None:
    """Single download attempt; raises FetchError on any failure."""
    existing = os.path.getsize(dest) if os.path.exists(dest) else 0
    headers = {"User-Agent": USER_AGENT}
    if existing and resume_allowed:
        headers["Range"] = f"bytes={existing}-"

    req = urllib.request.Request(url, headers=headers)
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=STREAM_TIMEOUT_S) as resp:
            status = resp.status
            if status == 416:
                log(f"    {label}: server reports range unsatisfiable ({format_bytes(existing)} "
                    f"already present) — keeping the partial file")
                return
            if status == 200:
                if existing:
                    log(f"    {label}: server ignored Range, restarting from zero")
                existing = 0
                mode = "wb"
            elif status == 206:
                mode = "ab"
            else:
                raise FetchError(f"unexpected HTTP status {status}")

            with open(dest, mode) as f:
                last_chunk_at = time.monotonic()
                first_chunk = True
                while True:
                    if time.monotonic() - started > FILE_DEADLINE_S:
                        raise FetchError("file deadline exceeded (90 min) — resume will continue later")
                    chunk = resp.read(CHUNK)
                    if not chunk:
                        break
                    now = time.monotonic()
                    if now - last_chunk_at > STALL_TIMEOUT_S:
                        raise FetchError(f"connection stalled (>{STALL_TIMEOUT_S}s without data)")
                    last_chunk_at = now
                    if first_chunk and mode == "wb" and looks_like_html(chunk):
                        raise FetchError(
                            "server returned an HTML page instead of the file "
                            "(mirror rate-limit or bot check?)"
                        )
                    first_chunk = False
                    f.write(chunk)
                    PROGRESS.report(label, f.tell(), expected_size, started)
    except urllib.error.HTTPError as exc:
        raise FetchError(f"HTTP {exc.code} {exc.reason}") from exc
    except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
        raise FetchError(f"connection error: {exc}") from exc

    PROGRESS.finish(label, os.path.getsize(dest), expected_size, started)


def fetch_one(name: str, url: str, md5: str | None, size: int | None, dest: str) -> None:
    """Fetch a zip, reusing a verified cache entry or resuming a partial file."""
    label = name
    if md5 and os.path.isfile(dest) and md5_of(dest) == md5:
        log(f"    {label}: already in cache, MD5 OK — skipping download")
        return

    download_file(url, dest, label, size)

    if md5 and md5_of(dest) != md5:
        log(f"    {label}: MD5 mismatch after download (got {md5_of(dest)}, expected {md5}) — "
            f"deleting and downloading fresh without resume")
        os.remove(dest)
        download_file(url, dest, label, size, resume_allowed=False)
        if md5_of(dest) != md5:
            raise FetchError(
                f"fetch-waydroid-images: MD5 mismatch for {name} after a clean re-download "
                f"(expected {md5}, got {md5_of(dest)}) — file corrupted or tampered, "
                f"SourceForge may be serving a bad copy; please re-run the build"
            )
    elif md5:
        log(f"    MD5 OK: {name}")
    else:
        log(f"    {name}: no MD5 in feed, skipping verification")


def safe_extractall(zip_path: str, dest_dir: str) -> None:
    """Zip-slip protection: reject entries with .. path components."""
    dest_real = os.path.realpath(dest_dir)
    with zipfile.ZipFile(zip_path) as zf:
        for member in zf.namelist():
            member_path = os.path.realpath(os.path.join(dest_real, member))
            if not (member_path == dest_real or member_path.startswith(dest_real + os.sep)):
                raise FetchError(
                    f"fetch-waydroid-images: suspicious path inside archive "
                    f"{os.path.basename(zip_path)}: {member!r} — escapes "
                    f"{dest_dir}, extraction aborted"
                )
        zf.extractall(dest_dir)


def run(args: argparse.Namespace) -> None:
    lineage = LINEAGE_VERSION[args.android_major]

    if args.android_major == "11":
        log("==> WARNING: upstream Waydroid has not released new lineage-18.1 "
            "(Android 11) builds since 2025-06-28 — the last available release "
            "will be used, this is expected.")

    system_pattern = rf"^lineage-{re.escape(lineage)}[._-].*-{args.variant}-waydroid_x86_64-system\.zip$"
    vendor_pattern = rf"^lineage-{re.escape(lineage)}[._-].*-MAINLINE-waydroid_x86_64-vendor\.zip$"

    log(f"==> Android {args.android_major} (lineage {lineage}, {args.variant}): searching for system image")
    system_root = sf_rss("/images/system/lineage/waydroid_x86_64", base=args.rss_base)
    system_name, system_url, system_md5, system_size = latest_matching(system_root, system_pattern)
    if not system_url:
        raise FetchError(
            f"fetch-waydroid-images: system image not found for Android "
            f"{args.android_major} ({args.variant}) — SourceForge structure "
            f"or file naming scheme may have changed, update the regex"
        )

    log(f"==> Android {args.android_major} (lineage {lineage}, MAINLINE): searching for vendor image")
    vendor_root = sf_rss("/images/vendor/waydroid_x86_64", base=args.rss_base)
    vendor_name, vendor_url, vendor_md5, vendor_size = latest_matching(vendor_root, vendor_pattern)
    if not vendor_url:
        raise FetchError(
            f"fetch-waydroid-images: vendor image not found for Android {args.android_major}"
        )

    log(f"    system: {system_name}" + (f" ({format_bytes(system_size)})" if system_size else ""))
    log(f"    vendor: {vendor_name}" + (f" ({format_bytes(vendor_size)})" if vendor_size else ""))

    os.makedirs(args.out, exist_ok=True)
    if args.cache_dir:
        os.makedirs(args.cache_dir, exist_ok=True)

    zip_sizes = [s for s in (system_size, vendor_size) if s]
    zip_dir = args.cache_dir
    if not zip_dir:
        zip_dir = tempfile.gettempdir()
    if zip_sizes:
        check_disk_space(zip_dir, sum(zip_sizes), "downloads (zip files)")

    def fetch_and_extract(zip_dir: str) -> None:
        system_zip = os.path.join(zip_dir, system_name)
        vendor_zip = os.path.join(zip_dir, vendor_name)

        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [
                pool.submit(fetch_one, system_name, system_url, system_md5, system_size, system_zip),
                pool.submit(fetch_one, vendor_name, vendor_url, vendor_md5, vendor_size, vendor_zip),
            ]
            for fut in futures:
                fut.result()

        check_unpack_space(args.out, [system_zip, vendor_zip])

        for zip_path in (system_zip, vendor_zip):
            safe_extractall(zip_path, args.out)

    if args.cache_dir:
        fetch_and_extract(args.cache_dir)
    else:
        with tempfile.TemporaryDirectory() as tmp:
            fetch_and_extract(tmp)

    for required in ("system.img", "vendor.img"):
        full = os.path.join(args.out, required)
        if not os.path.isfile(full):
            raise FetchError(
                f"fetch-waydroid-images: {full} not found after extraction — "
                f".zip contents do not match expectations (system.img/"
                f"vendor.img expected at archive root)"
            )

    receipt = {
        "schema_version": 1,
        "android_major": args.android_major,
        "variant": args.variant,
        "system_file": system_name,
        "vendor_file": vendor_name,
        "system_url": system_url,
        "vendor_url": vendor_url,
        "system_md5_verified": system_md5 is not None,
        "vendor_md5_verified": vendor_md5 is not None,
        "system_size_bytes": system_size,
        "vendor_size_bytes": vendor_size,
    }
    with open(os.path.join(args.out, "andler-fetch-manifest.json"), "w") as f:
        json.dump(receipt, f, indent=2, ensure_ascii=False)
        f.write("\n")

    if args.cache_dir:
        # Keep the cache bounded: only the two zips for the current build survive.
        for entry in os.listdir(args.cache_dir):
            entry_path = os.path.join(args.cache_dir, entry)
            if entry.endswith(".zip") and entry not in (system_name, vendor_name):
                os.remove(entry_path)

    log(f"==> Done: {os.path.join(args.out, 'system.img')}, {os.path.join(args.out, 'vendor.img')}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--android-major", required=True, choices=sorted(LINEAGE_VERSION))
    ap.add_argument("--variant", default="VANILLA", choices=["VANILLA", "GAPPS"])
    ap.add_argument("--out", default="/etc/waydroid-extra/images")
    ap.add_argument("--cache-dir", default=None,
                    help="persistent download cache (e.g. a BuildKit cache mount); "
                         "verified zips are reused across builds")
    ap.add_argument("--rss-base", default=SF_RSS_URL,
                    help=argparse.SUPPRESS)
    args = ap.parse_args()

    try:
        run(args)
    except FetchError as exc:
        print(f"{exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
