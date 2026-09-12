#!/usr/bin/env python3
"""Sanity-check a freshly built base image before build-base-image.yml
spends time compressing/splitting/publishing it. Two independent checks;
both run before anything gets published.

1. The qcow2 on disk actually hashes to what manifest.json's 'sha256'
   claims. Streamed in fixed-size chunks -- the inline check this
   replaces did `hashlib.sha256(open(path, 'rb').read())`, which reads
   the entire multi-GiB file into memory in one shot before hashing a
   single byte. That costs real RAM for zero benefit; every other hash in
   this codebase (add_parts_to_manifest.py's sha256_of) already streams.

2. manifest.json's 'android_images' names the Waydroid system/vendor zips
   that ended up in the image. check_waydroid_updates.py's needs_build()
   decides whether a rebuild is needed by substring-searching a
   previously-published manifest.json for these exact filenames. If this
   field is empty, every check finds "nothing published this", triggers a
   rebuild, and the entire point of that script -- avoid burning CI
   minutes and 2 GiB of Release storage on unchanged images -- silently
   stops working. Same principle already applied to the checker itself
   (a broken check is a red Actions status, not a quiet no-op forever):
   an empty android_images should be loud, not silent.

   If build-disk.sh already recorded these names itself, they're left
   untouched -- that's the more trustworthy source, since it reflects
   what fetch-waydroid-images.py actually downloaded during *this* build,
   not what a check run observed on SourceForge before the build started
   (a small race window: SourceForge could in principle publish something
   newer in between). If it's empty, the --system-image/--vendor-image
   values the triggering workflow_dispatch was called with (if any) are
   used to fill the gap. If it's *still* empty after that, this is a
   warning, not a hard failure -- a human-triggered backfill build with
   no inputs supplied is a normal thing to do and shouldn't be blocked.

Usage: verify_manifest.py <manifest.json> <qcow2> [--system-image NAME] [--vendor-image NAME]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys


def sha256_of(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("manifest", help="path to manifest.json")
    ap.add_argument("qcow2", help="path to the built qcow2 image")
    ap.add_argument("--system-image", default="", help="Waydroid system zip name, if known")
    ap.add_argument("--vendor-image", default="", help="Waydroid vendor zip name, if known")
    args = ap.parse_args()

    with open(args.manifest) as f:
        manifest = json.load(f)

    expected = manifest["sha256"]
    actual = sha256_of(args.qcow2)
    if actual != expected:
        print(f"::error::qcow2 sha256 mismatch: manifest says {expected}, actual file hashes to {actual}",
              file=sys.stderr)
        return 1
    print(f"qcow2 sha256 matches manifest.json: OK ({actual})")

    android_images = manifest.get("android_images") or {}
    if android_images:
        print(f"android_images already populated by the build itself: {android_images}")
    elif args.system_image or args.vendor_image:
        android_images = {"system": args.system_image, "vendor": args.vendor_image}
        manifest["android_images"] = android_images
        with open(args.manifest, "w") as f:
            json.dump(manifest, f, indent=2, ensure_ascii=False)
            f.write("\n")
        print(f"android_images was empty; filled from the triggering workflow_dispatch inputs: {android_images}")
    else:
        print(
            "::warning::android_images is empty and no --system-image/--vendor-image were given -- "
            "check_waydroid_updates.py will not be able to tell this release apart from 'never built' "
            "and will trigger this exact combination again next run. Fine for a one-off manual build; "
            "if this keeps happening on checker-triggered builds, build-disk.sh needs to record the "
            "Waydroid filenames it downloaded."
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
