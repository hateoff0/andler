#!/usr/bin/env python3
"""Check SourceForge for Waydroid system/vendor images newer than what this
repo has already published as a base-image-* GitHub Release, and trigger a
build for anything that changed.

Deliberately does NOT import docker/images/base/rootfs/.../fetch-waydroid-images.py:
this only needs the *latest filename* per version, not to download/verify/
extract anything, and staying decoupled means a change to one script can't
silently break the other.

Already published" is read back from the Releases themselves
(the most recent base-image---* release's manifest.json asset) rather than from a
separate state file kept in the repo -- one source of truth, same reasoning as the desired-state config:
a second, separately-updated record of "what we last built" can drift from reality, the release list cannot.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET

SF_PROJECT = "waydroid"
SF_RSS_URL = f"https://sourceforge.net/projects/{SF_PROJECT}/rss?path="

# Bare LineageOS version per android_major -- same name and same values as
# fetch-waydroid-images.py's own LINEAGE_VERSION, kept in sync with that
# file by hand since this script deliberately doesn't import it.
LINEAGE_VERSION = {
    "13": "20.0",
    "11": "18.1",
}
VARIANTS = ["VANILLA", "GAPPS"]

# Asymmetric on purpose: system images are nested under .../lineage/,
# vendor images are not (confirmed against fetch-waydroid-images.py
# directly). An earlier version of this script assumed a shared
# /images/{component}/lineage/waydroid_x86_64 template and 404'd on vendor
# every run -- this is that fix.
SYSTEM_RSS_PATH = "/images/system/lineage/waydroid_x86_64"
VENDOR_RSS_PATH = "/images/vendor/waydroid_x86_64"

REQUEST_TIMEOUT_S = 30
RSS_ATTEMPTS = 4
USER_AGENT = "andler-image-builder/1 (+https://github.com/andler-project/andler)"


def fetch_rss(path: str) -> ET.Element:
    """GET one SourceForge RSS feed, retrying transient failures.

    docker/images/base/rootfs/.../fetch-waydroid-images.py's own sf_rss()
    carries this exact comment: "single network failure on small RSS
    request breaks entire build" -- SourceForge's RSS endpoint is flaky
    enough in practice that a bare single attempt fails often, including
    with a plain 404 rather than a 5xx. This script deliberately does not
    import that function (see module docstring), but dropping its retry
    behavior while copying its URL scheme was a mistake: the flakiness is a
    property of the endpoint, not of that particular caller.
    """
    url = SF_RSS_URL + path
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    last_error: Exception | None = None
    for attempt in range(1, RSS_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_S) as resp:  # noqa: S310 (fixed sourceforge.net host)
                return ET.fromstring(resp.read())
        except (urllib.error.URLError, TimeoutError, ConnectionError, ET.ParseError) as exc:
            last_error = exc
            if attempt < RSS_ATTEMPTS:
                delay = 5 * attempt
                print(f"    RSS feed {path}: failed ({exc}), retrying in {delay}s...")
                time.sleep(delay)
    raise RuntimeError(f"failed to fetch RSS feed {path} after {RSS_ATTEMPTS} attempts: {last_error}")


def latest_matching(root: ET.Element, pattern: str) -> str | None:
    """Filename of the newest RSS entry whose basename matches pattern.

    Mirrors fetch-waydroid-images.py's own latest_matching(): SourceForge
    lists items newest-first, an item's title's last path segment is the
    real filename, and matching is a real anchored regex search -- not a
    loose substring check. Substring-on-lineage-tag alone can't tell a
    VANILLA system zip from a GAPPS one; they share the same tag.
    """
    rx = re.compile(pattern)
    for item in root.iter("item"):
        title = (item.findtext("title") or "").strip()
        name = title.rsplit("/", 1)[-1]
        if rx.search(name):
            return name
    return None


def latest_for(android_major: str) -> dict:
    """{'system': {'VANILLA': name|None, 'GAPPS': name|None}, 'vendor': name|None}

    Patterns copied verbatim from fetch-waydroid-images.py's system_pattern/
    vendor_pattern. Vendor has no VANILLA/GAPPS split -- it's always
    "MAINLINE" and shared by both variants, which is also why it's fetched
    once per android_major here, not once per variant.
    """
    lineage = LINEAGE_VERSION[android_major]

    system_root = fetch_rss(SYSTEM_RSS_PATH)
    system: dict[str, str | None] = {}
    for variant in VARIANTS:
        pattern = rf"^lineage-{re.escape(lineage)}[._-].*-{variant}-waydroid_x86_64-system\.zip$"
        system[variant] = latest_matching(system_root, pattern)

    vendor_root = fetch_rss(VENDOR_RSS_PATH)
    vendor_pattern = rf"^lineage-{re.escape(lineage)}[._-].*-MAINLINE-waydroid_x86_64-vendor\.zip$"
    vendor = latest_matching(vendor_root, vendor_pattern)

    return {"system": system, "vendor": vendor}


def published_manifest_text(repo: str, android_major: str, variant: str) -> str | None:
    """Raw manifest text of the most recent already-published release for
    this (android_major, variant), or None if never published.

    The pipeline publishes the manifest as `<stem>.manifest.json` (the stem
    every payload asset shares -- see base_image_download.rs and
    docs/ARCHITECTURE.md), not as a literal `manifest.json`, so the download
    globs for the suffix and reads whatever stem it finds."""
    prefix = f"base-image-android{android_major}-{variant.lower()}-"
    listing = subprocess.run(
        ["gh", "release", "list", "--repo", repo, "--limit", "100",
         "--json", "tagName,createdAt"],
        capture_output=True, text=True, check=True,
    )
    matches = [r for r in json.loads(listing.stdout) if r["tagName"].startswith(prefix)]
    if not matches:
        return None
    latest_tag = max(matches, key=lambda r: r["createdAt"])["tagName"]

    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["gh", "release", "download", latest_tag, "--repo", repo,
             "--pattern", "*.manifest.json", "--dir", tmp],
            check=True,
        )
        manifests = sorted(pathlib.Path(tmp).glob("*.manifest.json"))
        if len(manifests) != 1:
            raise RuntimeError(
                f"release {latest_tag} should carry exactly one "
                f"*.manifest.json asset, found {len(manifests)}"
            )
        return manifests[0].read_text()


def needs_build(wanted_filenames: list[str], published_text: str | None) -> bool:
    if published_text is None:
        return True
    # Substring match against the raw manifest text rather than a parsed
    # field name: the exact shape of the android_images receipt inside
    # manifest.json is an implementation detail of build-disk.sh/
    # fetch-waydroid-images.py this script does not need to know precisely
    # to answer "is this exact file already published". Robust to minor
    # schema changes on the manifest side; adjust if it ever proves wrong.
    return any(name not in published_text for name in wanted_filenames)


def trigger_build(repo: str, workflow: str, android_major: str, variant: str,
                   system_name: str, vendor_name: str, dry_run: bool) -> None:
    """Dispatch build-base-image.yml, passing along the exact system/vendor
    filenames this check just saw on SourceForge. build-base-image.yml's
    verify_manifest.py step uses these to fill manifest.json's
    'android_images' if build-disk.sh left it empty -- without this,
    published manifests never name what they contain, needs_build() below
    can never find a match, and every scheduled run rebuilds everything
    again regardless of whether anything actually changed."""
    print(f"  -> {'[dry-run] would trigger' if dry_run else 'triggering'} "
          f"build: android{android_major} {variant}")
    if dry_run:
        return
    subprocess.run(
        ["gh", "workflow", "run", workflow, "--repo", repo,
         "-f", f"android_major={android_major}",
         "-f", f"android_variant={variant}",
         "-f", f"system_image_name={system_name}",
         "-f", f"vendor_image_name={vendor_name}"],
        check=True,
    )


def write_step_summary(rows: list[tuple[str, str, str, str]]) -> None:
    """Append a small markdown table to $GITHUB_STEP_SUMMARY, if set (it
    isn't when running this script outside Actions, e.g. locally). Lets
    anyone glance at the Actions run and see what was checked without
    opening the log -- exactly the kind of visibility this script's own
    docstring says a silent SourceForge/RSS failure needs, applied here to
    the routine, non-failure case too."""
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path:
        return
    with open(summary_path, "a") as f:
        f.write("### Waydroid image check\n\n")
        f.write("| Track | Variant | Result | Latest system image |\n")
        f.write("|---|---|---|---|\n")
        for track, variant, result, detail in rows:
            f.write(f"| {track} | {variant} | {result} | `{detail}` |\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", required=True, help="owner/repo, e.g. from ${{ github.repository }}")
    ap.add_argument("--trigger-workflow", required=True, help="workflow filename to dispatch, e.g. build-base-image.yml")
    ap.add_argument("--dry-run", action="store_true", help="print what would happen, trigger nothing")
    args = ap.parse_args()

    any_triggered = False
    had_error = False
    summary_rows: list[tuple[str, str, str, str]] = []

    for android_major in LINEAGE_VERSION:
        track = f"android{android_major}"
        try:
            current = latest_for(android_major)
        except Exception as exc:  # exhausted retries inside fetch_rss: a real, persistent failure
            print(f"::error::android{android_major}: failed to check SourceForge RSS: {exc}")
            had_error = True
            summary_rows.append((track, "—", "❌ check failed", str(exc)[:80]))
            continue  # android11/13 are independent checks; one failing shouldn't hide the other

        if not current["vendor"]:
            print(f"::warning::android{android_major}: no vendor image matched for this lineage "
                  f"version (feed layout may have changed) -- skipping this version this run")
            summary_rows.append((track, "—", "⚠️ no vendor match", ""))
            continue

        for variant in VARIANTS:
            system_name = current["system"].get(variant)
            if not system_name:
                print(f"::warning::android{android_major} {variant}: no system image matched "
                      f"-- skipping")
                summary_rows.append((track, variant, "⚠️ no system match", ""))
                continue

            print(f"android{android_major} {variant}: system={system_name!r} vendor={current['vendor']!r}")
            published_text = published_manifest_text(args.repo, android_major, variant)
            if needs_build([system_name, current["vendor"]], published_text):
                status = "never built" if published_text is None else "update available"
                print(f"  -> NEW ({status})")
                any_triggered = True
                trigger_build(args.repo, args.trigger_workflow, android_major, variant,
                               system_name, current["vendor"], args.dry_run)
                result = "🔍 would trigger" if args.dry_run else "🔨 triggered"
                summary_rows.append((track, variant, f"{result} ({status})", system_name))
            else:
                print("  -> up to date, nothing to do")
                summary_rows.append((track, variant, "✅ up to date", system_name))

    write_step_summary(summary_rows)

    if not any_triggered and not had_error:
        print("Nothing new on SourceForge -- no builds triggered.")
    return 1 if had_error else 0


if __name__ == "__main__":
    sys.exit(main())
