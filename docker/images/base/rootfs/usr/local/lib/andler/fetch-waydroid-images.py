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
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

SF_PROJECT = "waydroid"

# lineage-18.1.x == Android 11, lineage-20.0.x == Android 13
LINEAGE_VERSION = {
    "11": "18.1",
    "13": "20.0",
}

USER_AGENT = "andler-image-builder/1 (+https://github.com/andler-project/andler)"


def sf_rss(path: str, attempts: int = 3) -> ET.Element:
    # Retry: single network failure on small RSS request breaks entire build
    url = f"https://sourceforge.net/projects/{SF_PROJECT}/rss?path={path}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    last_error: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = resp.read()
            return ET.fromstring(data)
        except (urllib.error.URLError, TimeoutError, ConnectionError, ET.ParseError) as exc:
            last_error = exc
            if attempt < attempts:
                delay = 3 * attempt
                print(f"    RSS feed {path}: failed ({exc}), retrying in {delay}s...")
                time.sleep(delay)
    raise SystemExit(f"fetch-waydroid-images: failed to fetch RSS feed {path}: {last_error}")


def latest_matching(path: str, pattern: str) -> tuple[str | None, str | None, str | None, int | None]:
    """First file in RSS feed of directory path matching pattern."""
    root = sf_rss(path)
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


def verify_md5(path: str, expected_md5: str) -> None:
    """Compare locally computed MD5 with SourceForge-provided checksum."""
    hasher = hashlib.md5()
    with open(path, "rb") as f:
        while chunk := f.read(1024 * 1024):
            hasher.update(chunk)
    actual = hasher.hexdigest().lower()
    if actual != expected_md5:
        raise SystemExit(
            f"fetch-waydroid-images: MD5 mismatch for {os.path.basename(path)} "
            f"(expected {expected_md5}, got {actual}) — file corrupted or tampered, "
            f"please re-run the build"
        )
    print(f"    MD5 OK: {os.path.basename(path)}")


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
        raise SystemExit(
            f"fetch-waydroid-images: insufficient space for {label} "
            f"on {check_path} — need ~{needed // (1024**2)}MB, "
            f"have {free // (1024**2)}MB"
        )


def download(url: str, dest: str, attempts: int = 4) -> None:
    """Download with retries and exponential backoff."""
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    last_error: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            print(f"    downloading (attempt {attempt}/{attempts}): {url}")
            with urllib.request.urlopen(req, timeout=600) as resp, open(dest, "wb") as f:
                while chunk := resp.read(1024 * 1024):
                    f.write(chunk)
            return
        except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
            last_error = exc
            if attempt < attempts:
                delay = 5 * attempt
                print(f"    download failed ({exc}), retrying in {delay}s...")
                time.sleep(delay)
    raise SystemExit(f"fetch-waydroid-images: failed to download {url}: {last_error}")


def safe_extractall(zip_path: str, dest_dir: str) -> None:
    """Zip-slip protection: reject entries with .. path components."""
    dest_real = os.path.realpath(dest_dir)
    with zipfile.ZipFile(zip_path) as zf:
        for member in zf.namelist():
            member_path = os.path.realpath(os.path.join(dest_real, member))
            if not (member_path == dest_real or member_path.startswith(dest_real + os.sep)):
                raise SystemExit(
                    f"fetch-waydroid-images: suspicious path inside archive "
                    f"{os.path.basename(zip_path)}: {member!r} — escapes "
                    f"{dest_dir}, extraction aborted"
                )
        zf.extractall(dest_dir)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--android-major", required=True, choices=sorted(LINEAGE_VERSION))
    ap.add_argument("--variant", default="VANILLA", choices=["VANILLA", "GAPPS"])
    ap.add_argument("--out", default="/etc/waydroid-extra/images")
    args = ap.parse_args()

    lineage = LINEAGE_VERSION[args.android_major]

    if args.android_major == "11":
        print(
            "==> WARNING: upstream Waydroid has not released new lineage-18.1 "
            "(Android 11) builds since 2025-06-28 — the last available release "
            "will be used, this is expected.",
            file=sys.stderr,
        )

    system_pattern = rf"^lineage-{re.escape(lineage)}[._-].*-{args.variant}-waydroid_x86_64-system\.zip$"
    vendor_pattern = rf"^lineage-{re.escape(lineage)}[._-].*-MAINLINE-waydroid_x86_64-vendor\.zip$"

    print(f"==> Android {args.android_major} (lineage {lineage}, {args.variant}): searching for system image")
    system_name, system_url, system_md5, system_size = latest_matching(
        "/images/system/lineage/waydroid_x86_64", system_pattern
    )
    if not system_url:
        sys.exit(
            f"fetch-waydroid-images: system image not found for Android "
            f"{args.android_major} ({args.variant}) — SourceForge structure "
            f"or file naming scheme may have changed, update the regex"
        )

    print(f"==> Android {args.android_major} (lineage {lineage}, MAINLINE): searching for vendor image")
    vendor_name, vendor_url, vendor_md5, vendor_size = latest_matching(
        "/images/vendor/waydroid_x86_64", vendor_pattern
    )
    if not vendor_url:
        sys.exit(
            f"fetch-waydroid-images: vendor image not found for Android {args.android_major}"
        )

    print(f"    system: {system_name}" + (f" (md5: {system_md5})" if system_md5 else " (md5 not in feed)"))
    print(f"    vendor: {vendor_name}" + (f" (md5: {vendor_md5})" if vendor_md5 else " (md5 not in feed)"))

    os.makedirs(args.out, exist_ok=True)

    tmp_root = tempfile.gettempdir()
    if system_size:
        check_disk_space(tmp_root, system_size, "system.zip (temp directory)")
        check_disk_space(args.out, system_size, "unpacked system.img")
    if vendor_size:
        check_disk_space(tmp_root, vendor_size, "vendor.zip (temp directory)")
        check_disk_space(args.out, vendor_size, "unpacked vendor.img")

    with tempfile.TemporaryDirectory() as tmp:
        system_zip = os.path.join(tmp, system_name)
        vendor_zip = os.path.join(tmp, vendor_name)
        download(system_url, system_zip)
        download(vendor_url, vendor_zip)

        if system_md5:
            verify_md5(system_zip, system_md5)
        else:
            print("    warning: MD5 for system image not found in feed, skipping verification")
        if vendor_md5:
            verify_md5(vendor_zip, vendor_md5)
        else:
            print("    warning: MD5 for vendor image not found in feed, skipping verification")

        for zip_path in (system_zip, vendor_zip):
            safe_extractall(zip_path, args.out)

    for required in ("system.img", "vendor.img"):
        full = os.path.join(args.out, required)
        if not os.path.isfile(full):
            sys.exit(
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

    print(f"==> Done: {args.out}/system.img, {args.out}/vendor.img")


if __name__ == "__main__":
    main()
