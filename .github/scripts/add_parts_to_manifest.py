#!/usr/bin/env python3
"""Add a 'parts' list (one entry per split file, each with its own sha256
and size) to an existing base-image manifest.json.

Usage: add_parts_to_manifest.py <manifest.json> <part1> [<part2> ...]

Extends the manifest build-disk.sh already writes (schema_version,
sha256, file_size_bytes, android_major, ...) rather than inventing a new
format -- the CLI downloader (once written) reads one manifest shape for
both a locally-built image and a remotely-published one.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys


def sha256_of(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <manifest.json> <part1> [<part2> ...]", file=sys.stderr)
        return 2

    manifest_path, *part_paths = sys.argv[1:]

    with open(manifest_path) as f:
        manifest = json.load(f)

    manifest["compression"] = "zstd"
    manifest["parts"] = [
        {
            "name": os.path.basename(p),
            "sha256": sha256_of(p),
            "size_bytes": os.path.getsize(p),
        }
        for p in sorted(part_paths)
    ]

    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
        f.write("\n")

    total = sum(part["size_bytes"] for part in manifest["parts"])
    print(f"manifest updated: {len(manifest['parts'])} part(s), {total / (1 << 30):.2f} GiB total")
    return 0


if __name__ == "__main__":
    sys.exit(main())
