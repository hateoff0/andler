#!/usr/bin/env python3
"""Check SourceForge for Waydroid system/vendor images newer than what this
repo has already published as a base-image-* GitHub Release, and trigger a
build for anything that changed.

Deliberately does NOT import docker/images/base/rootfs/.../fetch-waydroid-images.py:
this only needs the *latest filename* per version, not to download/verify/
extract anything, and staying decoupled means a change to one script can't
silently break the other.

"Already published" is read back from the Releases themselves (the most
recent base-image-<major>-<variant>-* release's manifest.json asset) rather
than from a separate state file kept in the repo -- one source of truth,
same reasoning as PLAN.md section 1 (desired-state config): a second,
separately-updated record of "what we last built" can drift from reality,
the release list cannot.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

SF_PROJECT = "waydroid"
SF_RSS_URL = f"https://sourceforge.net/projects/{SF_PROJECT}/rss?path="

# android_major -> substring that identifies that LineageOS line in the
# SourceForge filenames. Android 11 (lineage-18.1) is frozen upstream but
# still checked -- if it ever resumes, this picks it up with no code change.
LINEAGE_TAG = {
    "13": "lineage-20.0",
    "11": "lineage-18.1",
}
COMPONENTS = ["system", "vendor"]
VARIANTS = ["VANILLA", "GAPPS"]
REQUEST_TIMEOUT_S = 30


def fetch_rss(path: str) -> ET.Element:
    url = SF_RSS_URL + path
    req = urllib.request.Request(url, headers={"User-Agent": "andler-ci-check/1"})
    with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_S) as resp:  # noqa: S310 (fixed sourceforge.net host)
        return ET.fromstring(resp.read())


def latest_matching(root: ET.Element, version_tag: str) -> str | None:
    """Filename of the newest RSS entry whose title contains version_tag.
    SourceForge lists release-directory RSS items newest-first, so the
    first match is the latest release for that line."""
    for item in root.iter("item"):
        title = (item.findtext("title") or "").strip()
        if version_tag in title:
            return title.lstrip("/").split("/")[-1]
    return None


def latest_for(android_major: str) -> dict[str, str | None]:
    version_tag = LINEAGE_TAG[android_major]
    result: dict[str, str | None] = {}
    for component in COMPONENTS:
        root = fetch_rss(f"/images/{component}/lineage/waydroid_x86_64")
        result[component] = latest_matching(root, version_tag)
    return result


def published_manifest_text(repo: str, android_major: str, variant: str) -> str | None:
    """Raw manifest.json text of the most recent already-published release
    for this (android_major, variant), or None if never published."""
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
             "--pattern", "manifest.json", "--dir", tmp],
            check=True,
        )
        return (pathlib.Path(tmp) / "manifest.json").read_text()


def needs_build(current: dict[str, str | None], published_text: str | None) -> bool:
    if published_text is None:
        return True
    # Substring match against the raw manifest text rather than a parsed
    # field name: the exact shape of the android_images receipt inside
    # manifest.json is an implementation detail of build-disk.sh/
    # fetch-waydroid-images.py this script does not need to know precisely
    # to answer "is this exact file already published". Robust to minor
    # schema changes on the manifest side; adjust if it ever proves wrong.
    return any(name and name not in published_text for name in current.values())


def trigger_build(repo: str, workflow: str, android_major: str, variant: str, dry_run: bool) -> None:
    print(f"  -> {'[dry-run] would trigger' if dry_run else 'triggering'} "
          f"build: android{android_major} {variant}")
    if dry_run:
        return
    subprocess.run(
        ["gh", "workflow", "run", workflow, "--repo", repo,
         "-f", f"android_major={android_major}",
         "-f", f"android_variant={variant}"],
        check=True,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", required=True, help="owner/repo, e.g. from ${{ github.repository }}")
    ap.add_argument("--trigger-workflow", required=True, help="workflow filename to dispatch, e.g. build-base-image.yml")
    ap.add_argument("--dry-run", action="store_true", help="print what would happen, trigger nothing")
    args = ap.parse_args()

    any_triggered = False
    for android_major in LINEAGE_TAG:
        try:
            current = latest_for(android_major)
        except Exception as exc:  # network/parse failure: fail loudly, do not treat as "nothing new"
            print(f"::error::android{android_major}: failed to check SourceForge RSS: {exc}")
            return 1

        if not current.get("system") or not current.get("vendor"):
            print(f"::warning::android{android_major}: no matching system/vendor entry in the "
                  f"RSS feed (feed layout may have changed) -- skipping this version this run")
            continue

        print(f"android{android_major}: latest system={current['system']!r} vendor={current['vendor']!r}")

        for variant in VARIANTS:
            published_text = published_manifest_text(args.repo, android_major, variant)
            if needs_build(current, published_text):
                status = "never built" if published_text is None else "update available"
                print(f"  {variant}: NEW ({status})")
                any_triggered = True
                trigger_build(args.repo, args.trigger_workflow, android_major, variant, args.dry_run)
            else:
                print(f"  {variant}: up to date, nothing to do")

    if not any_triggered:
        print("Nothing new on SourceForge -- no builds triggered.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
