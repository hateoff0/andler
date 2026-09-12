#!/usr/bin/env python3
"""Render the markdown body for a base-image GitHub Release.

The same output also gets appended to the workflow run's
$GITHUB_STEP_SUMMARY (see build-base-image.yml) -- one template, two
destinations, generated from manifest.json plus the handful of values
only the workflow itself knows (repo, run URL). Kept separate from
add_parts_to_manifest.py / verify_manifest.py, which mutate the manifest;
this script only reads it, run last, after every other field is final.

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

    original = m["file_size_bytes"]
    compressed = m.get("total_compressed_size_bytes")
    ratio = m.get("compression_ratio_pct")
    n_parts = len(m.get("parts", []))

    size_line = human_gib(original) + " uncompressed"
    if compressed is not None:
        size_line += f", {human_gib(compressed)} to download across {n_parts} part(s)"
        if ratio is not None:
            size_line += f" ({ratio}% smaller)"

    run_label = f"#{args.run_number}" if args.run_number else "this run"
    commit_url = f"https://github.com/{args.repo}/commit/{m['git_rev']}"
    rough_needed_gib = math.ceil((original + (compressed or original)) / GIB)

    lines = [
        f"## 📦 andler base image — Android {m['android_major']} ({m['android_variant']})",
        "",
        f"Automated build from [`{m['git_rev']}`]({commit_url}), built {m['built_at']}.",
        "",
        "| | |",
        "|---|---|",
        f"| Waydroid system image | `{system_name}` |",
        f"| Waydroid vendor image | `{vendor_name}` |",
        f"| Image size | {size_line} |",
        f"| Workflow run | [{run_label}]({args.run_url}) |",
    ]
    if args.compress_seconds is not None:
        level = m.get("zstd_level", "?")
        lines.append(f"| Compression | {args.compress_seconds}s at zstd level {level} |")

    lines += [
        "",
        "### ⬇️ Download & verify",
        "",
        "Put `manifest.json`, `SHA256SUMS`, and every `*.part` file from this release into "
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
        "(compressed and decompressed image on disk at once).",
        "",
        "---",
        f"Unattended build — check the [workflow run]({args.run_url}) logs before trusting "
        "this image if anything above looks wrong.",
    ]
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
