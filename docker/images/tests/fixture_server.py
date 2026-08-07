#!/usr/bin/env python3
"""Range-capable fixture HTTP server for fetch-waydroid-images tests.

Serves files from --directory; every request is logged verbatim to stdout as
"METHOD path status range=..." (the test harness greps the log to prove that
requests were/weren't made and that Range resumption was used).
"""
from __future__ import annotations

import argparse
import os
import re
import http.server


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def __init__(self, request, client_address, server, directory):
        self.directory = directory
        super().__init__(request, client_address, server)

    def do_GET(self) -> None:
        path = self.path.split("?", 1)[0]
        if path.endswith("/download"):
            path = path[: -len("/download")]
        fs_path = os.path.join(self.directory, path.lstrip("/"))
        if not os.path.isfile(fs_path):
            self.send_error(404)
            return
        size = os.path.getsize(fs_path)
        start, end = 0, size
        status = 200
        match = re.match(r"bytes=(\d+)-(\d*)", self.headers.get("Range", "").strip())
        if match:
            start = int(match.group(1))
            if match.group(2):
                end = min(int(match.group(2)) + 1, size)
            if start >= size:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.end_headers()
                print(f"GET {self.path} 416 range={self.headers.get('Range','')}", flush=True)
                return
            status = 206
        with open(fs_path, "rb") as f:
            f.seek(start)
            body = f.read(end - start)
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(body)))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{start + len(body) - 1}/{size}")
        self.end_headers()
        self.wfile.write(body)
        print(f"GET {self.path} {status} range={self.headers.get('Range','')}", flush=True)

    def log_message(self, fmt: str, *args) -> None:
        pass


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--directory", required=True)
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    handler = lambda *a, **kw: Handler(*a, directory=args.directory, **kw)
    http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler).serve_forever()


if __name__ == "__main__":
    main()