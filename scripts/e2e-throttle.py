#!/usr/bin/env python3
"""Run a signed-out GUI E2E with one real HTTP 429 before public API forwarding.

YTFAST_E2E_BINARY=/path/to/ytfast python3 scripts/e2e-throttle.py library
Receipt: artifacts/e2e-throttle/<UTC>/receipt.json. No request bodies are saved.
"""
import datetime
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

lock = threading.Lock()
requests = []
started = time.monotonic()

class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.headers.get("Cookie") or self.headers.get("Authorization"):
            self.send_error(403, "This scenario must be signed out")
            return
        with lock:
            first = not requests
            entry = {"path": self.path, "at": time.monotonic() - started, "status": 429 if first else None}
            requests.append(entry)
        if first:
            self.send_response(429)
            self.send_header("Retry-After", "1")
            self.end_headers()
            return
        request = urllib.request.Request("https://music.youtube.com" + self.path, body, {
            "Content-Type": "application/json", "Origin": "https://music.youtube.com",
            "Referer": "https://music.youtube.com/", "User-Agent": "Mozilla/5.0",
        })
        try:
            response = urllib.request.urlopen(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        except Exception:
            self.send_error(502)
            return
        with response:
            data = response.read()
            entry["status"] = response.status
            self.send_response(response.status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
threading.Thread(target=server.serve_forever, daemon=True).start()
scenario = sys.argv[1] if len(sys.argv) > 1 else "library"
if scenario != "library":
    raise SystemExit("Use the signed-out library scenario")
env = dict(os.environ, YTFAST_E2E_API_ORIGIN=f"http://127.0.0.1:{server.server_port}")
result = subprocess.run(["sh", "scripts/e2e.sh", scenario], env=env, check=False)
server.shutdown()
with lock:
    retried = bool(requests) and any(r["path"] == requests[0]["path"] and r["at"] - requests[0]["at"] >= 0.95 and r["status"] == 200 for r in requests[1:])
    receipt = {"requests": requests, "retry_after_respected": retried, "gui_exit": result.returncode}
out = Path("artifacts/e2e-throttle") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
out.mkdir(parents=True, exist_ok=True)
(out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(f"Throttle receipt: {out}/receipt.json")
raise SystemExit(result.returncode or (0 if retried else 1))
