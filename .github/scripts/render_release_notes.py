#!/usr/bin/env python3
"""Render the markdown body for a base-image GitHub Release.

The same output also gets appended to the workflow run's
$GITHUB_STEP_SUMMARY (see build-base-image.yml) -- one template, two
destinations, generated from manifest.json plus the handful of values
only the workflow itself knows (repo, run URL). Kept separate from
add_parts_to_manifest.py / verify_manifest.py, which mutate the manifest;
this script only reads it, run last, after every other field is final.

The body leads with what a reader can do with the image (one command, no
rootfs build), keeps the build's provenance in a table, and hides the manual
checksum walkthrough behind a collapsed block -- mirrors the project README's
shape rather than opening with a wall of shell.

Usage:
  render_release_notes.py <manifest.json> --repo owner/repo --run-url URL
      [--run-number N] [--compress-seconds N]
"""

from __future__ import annotations

import argparse
import json
import math

GIB = 1 << 30


def human_gib(num_bytes: int) -> str:
    return f"{num_bytes / GIB:.2f} GiB"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("manifest")
    ap.add_argument("--repo", required=True)
    ap.add_argument("--run-url", required=True)
    ap.add_argument("--run-number", default="")
    ap.add_argument("--compress-seconds", type=int, default=None)
    args = ap.parse_args()

    with open(args.manifest) as f:
        m = json.load(f)

    android_images = m.get("android_images") or {}
    system_name = android_images.get("system") or "unknown"
    vendor_name = android_images.get("vendor") or "unknown"

    major = m["android_major"]
    variant = m["android_variant"]
    variant_lower = variant.lower()

    original = m["file_size_bytes"]
    compressed = m.get("total_compressed_size_bytes")
    ratio = m.get("compression_ratio_pct")
    n_parts = len(m.get("parts", []))

    size_line = human_gib(original) + " uncompressed"
    if compressed is not None:
        size_line += f" · {human_gib(compressed)} to download across {n_parts} part(s)"
        if ratio is not None:
            size_line += f" ({ratio}% smaller)"

    run_label = f"#{args.run_number}" if args.run_number else "this run"
    commit_url = f"https://github.com/{args.repo}/commit/{m['git_rev']}"
    rough_needed_gib = math.ceil((original + (compressed or original)) / GIB)

    lines = [
        f"## 📦 base image · android {major} {variant_lower}",
        "",
        f"Bootable Waydroid base image for guests on **Android {major}**, built and "
        "checksummed by CI — no rootfs build, no image assembly on your side.",
        "",
        "| | |",
        "| :--- | :--- |",
        f"| **Android** | {major} · {variant} |",
        f"| **Waydroid system image** | `{system_name}` |",
        f"| **Waydroid vendor image** | `{vendor_name}` |",
        f"| **Size** | {size_line} |",
        f"| **Built from** | [`{m['git_rev']}`]({commit_url}) · {m['built_at']} |",
        f"| **Workflow run** | [{run_label}]({args.run_url}) |",
    ]
    if args.compress_seconds is not None:
        level = m.get("zstd_level", "?")
        lines.append(f"| **Compression** | zstd level {level} in {args.compress_seconds}s |")

    lines += [
        "",
        "### 🚀 Use it",
        "",
        "```bash",
        f"andler image download --android-version {major} --variant {variant_lower}",
        f"andler create --kind android --name pixel --android-version {major}",
        "```",
        "",
        "`image download` verifies every part against this release's manifest, unpacks "
        "the zstd stream and installs the image into `~/.andler/cache/base-images/` — an "
        "already-cached build is reused without another request. `andler image list` shows "
        "what is published, and what is already local.",
        "",
        "<details>",
        "<summary><strong>Download and verify by hand</strong></summary>",
        "",
        "Put this release's `*.manifest.json`, `SHA256SUMS` and every `*.part` file into "
        "one empty folder, then:",
        "",
        "```bash",
        "sha256sum -c SHA256SUMS",
        "cat *.qcow2.zst.*.part > image.qcow2.zst",
    ]
    if m.get("total_sha256"):
        lines.append(f'echo "{m["total_sha256"]}  image.qcow2.zst" | sha256sum -c -')
    lines += [
        "zstd -d --long=27 image.qcow2.zst -o image.qcow2",
        f'echo "{m["sha256"]}  image.qcow2" | sha256sum -c -',
        "```",
        "",
        f"Roughly {rough_needed_gib} GB of free disk space covers the last two steps "
        "(the compressed and the unpacked image are on disk at once).",
        "",
        "</details>",
        "",
        "> [!NOTE]",
        f"> Unattended build. Every value above comes from the build's own manifest — if "
        f"something looks off, read the [workflow run]({args.run_url}) logs before trusting "
        "this image.",
        "",
        "---",
        "",
        f"<sub>[README](https://github.com/{args.repo}#readme) · "
        f"[Quick start](https://github.com/{args.repo}#-quick-start) · "
        f"[Docs](https://github.com/{args.repo}/tree/main/docs) · "
        f"[Changelog](https://github.com/{args.repo}/blob/main/docs/CHANGELOG.md)</sub>",
    ]
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
