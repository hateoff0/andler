#!/usr/bin/env python3
"""Add a 'parts' list (one entry per split file, each with its own sha256
and size) to an existing base-image manifest.json, plus the fields needed
to verify and describe the split as a whole:

- total_sha256: sha256 of the parts concatenated in numeric order, i.e.
  of the compressed .zst blob before it was split -- computed by
  streaming each part into one running hash object, without ever writing
  the concatenation to disk. This was in the original plan for this
  manifest extension but never actually got added; the CLI downloader
  (and anyone verifying by hand) needs it to check the reassembled blob
  in one step, before spending time on decompression.
- total_compressed_size_bytes / compression_ratio_pct: for release notes,
  and, over successive builds, for judging whether the zstd level in use
  is worth its CPU time.

Also writes a sibling SHA256SUMS file next to manifest.json, in the
standard `sha256sum(1)` checksum-file format ("<hash>  <filename>", one
line per part) -- downloading the release and running
`sha256sum -c SHA256SUMS` then needs no custom tooling at all.

Usage: add_parts_to_manifest.py <manifest.json> <part1> [<part2> ...] [--zstd-level N]

Extends the manifest build-disk.sh already writes (schema_version,
sha256, file_size_bytes, android_major, ...) rather than inventing a new
format -- the CLI downloader (once written) reads one manifest shape for
both a locally-built image and a remotely-published one.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os

GIB = 1 << 30


def hash_parts(paths: list[str]) -> tuple[dict[str, str], str]:
    """Per-file sha256 for each path, and the sha256 of all of them
    concatenated in the given order -- reading each file exactly once
    (one pass, two hash objects fed from the same chunks) rather than
    hashing every part individually and then re-reading them all again
    to get the combined hash."""
    per_file: dict[str, str] = {}
    combined = hashlib.sha256()
    for p in paths:
        h = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
                combined.update(chunk)
        per_file[p] = h.hexdigest()
    return per_file, combined.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("manifest", help="path to manifest.json")
    ap.add_argument("parts", nargs="+", help="the split part files, any order")
    ap.add_argument("--zstd-level", type=int, default=None,
                    help="zstd level used to compress, recorded for future tuning")
    args = ap.parse_args()

    manifest_path = args.manifest
    part_paths = sorted(args.parts)  # lexicographic == numeric for split -d's 00, 01, ...

    with open(manifest_path) as f:
        manifest = json.load(f)

    per_file_sha256, total_sha256 = hash_parts(part_paths)

    manifest["compression"] = "zstd"
    if args.zstd_level is not None:
        manifest["zstd_level"] = args.zstd_level
    manifest["parts"] = [
        {
            "name": os.path.basename(p),
            "sha256": per_file_sha256[p],
            "size_bytes": os.path.getsize(p),
        }
        for p in part_paths
    ]

    total_compressed = sum(part["size_bytes"] for part in manifest["parts"])
    manifest["total_compressed_size_bytes"] = total_compressed
    manifest["total_sha256"] = total_sha256

    original = manifest.get("file_size_bytes")
    if isinstance(original, int) and original > 0:
        manifest["compression_ratio_pct"] = round((1 - total_compressed / original) * 100, 2)

    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
        f.write("\n")

    sums_path = os.path.join(os.path.dirname(manifest_path) or ".", "SHA256SUMS")
    with open(sums_path, "w") as f:
        for part in manifest["parts"]:
            f.write(f"{part['sha256']}  {part['name']}\n")

    print(f"manifest updated: {len(manifest['parts'])} part(s), {total_compressed / GIB:.2f} GiB total")
    if "compression_ratio_pct" in manifest:
        print(f"compression: {original / GIB:.2f} GiB -> {total_compressed / GIB:.2f} GiB "
              f"({manifest['compression_ratio_pct']}% smaller)")
    print(f"wrote {sums_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
