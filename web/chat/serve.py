#!/usr/bin/env python3
"""Serve the web chat panel, with a `tetanus serve` behind it.

The panel is three static files and needs no build step, but it does need a
running WebSocket carrier, and the port that carrier ends up on is not known
until it has bound one. So this starts `tetanus serve --listen`, reads the
address out of its banner, and hands that address to the page.

Nothing here is part of the product: it is the development server that makes
the panel openable in one command. The page it serves is the file in the
repository, unmodified apart from the line naming the carrier.

Usage: python3 web/chat/serve.py [--host 0.0.0.0] [--port 5300] [--dir DIR]
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
#: The address a reviewer opens. This server binds every interface, so the line
#: it prints has to name the one they can reach: `localhost` is only true on
#: the machine the server runs on, and this is not read there.
PUBLIC_HOST = "15.204.113.4"
WILDCARD = ("0.0.0.0", "::", "")
ADDRESS = re.compile(r"^address\s+(\S+)\s*$")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def binary() -> Path:
    """The `tetanus` to run. Built on demand, so a clean clone works."""
    built = ROOT / "target" / "debug" / "tetanus"
    if not built.exists():
        print("building tetanus…", file=sys.stderr)
        subprocess.run(
            ["cargo", "build", "-p", "tetanus-hardness", "--bin", "tetanus"],
            cwd=ROOT, check=True,
        )
    return built


def carrier(sessions: Path) -> tuple[subprocess.Popen, int]:
    """Start the WebSocket carrier and return it with the port it bound.

    Port 0 asks the operating system for a free one, which is why the banner
    has to be read rather than assumed.
    """
    served = subprocess.Popen(
        [str(binary()), "serve", "--dir", str(sessions), "--listen", "0.0.0.0:0"],
        stderr=subprocess.PIPE, text=True, cwd=ROOT,
    )
    for line in served.stderr:
        found = ADDRESS.match(ANSI.sub("", line))
        if found:
            threading.Thread(target=drain, args=(served,), daemon=True).start()
            return served, int(found.group(1).rsplit(":", 1)[1])
    raise SystemExit("tetanus serve stopped before it announced an address")


def drain(served: subprocess.Popen) -> None:
    """Pass the carrier's remaining output through, so its errors are visible."""
    for line in served.stderr:
        sys.stderr.write(f"[tetanus serve] {line}")


def page(ws_port: int) -> bytes:
    """The panel, told where its carrier is.

    The address is built in the browser from the host the page was loaded
    from, so the same server works over localhost, over a LAN address and
    through a tunnel without being told which.
    """
    told = (
        "<script>window.TETANUS_WS = "
        f'`ws://${{location.hostname}}:{ws_port}`;</script>\n</head>'
    )
    return (HERE / "index.html").read_text().replace("</head>", told, 1).encode()


def serve(host: str, port: int, ws_port: int) -> None:
    body = page(ws_port)
    script = (HERE / "chat.js").read_bytes()

    class Panel(BaseHTTPRequestHandler):
        def log_message(self, *args) -> None:  # quiet: the carrier's log is the interesting one
            pass

        def do_GET(self) -> None:
            asked = self.path.split("?", 1)[0]
            if asked in ("/", "/index.html"):
                self.reply(200, "text/html; charset=utf-8", body)
            elif asked == "/chat.js":
                self.reply(200, "text/javascript; charset=utf-8", script)
            else:
                self.reply(404, "text/plain; charset=utf-8", b"no such page\n")

        def reply(self, status: int, kind: str, said: bytes) -> None:
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(said)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(said)

    server = ThreadingHTTPServer((host, port), Panel)
    shown = PUBLIC_HOST if host in WILDCARD else host
    print(f"panel    http://{shown}:{port}", file=sys.stderr)
    print(f"carrier  ws://{shown}:{ws_port}", file=sys.stderr)
    print("note: end with Ctrl-C", file=sys.stderr)
    server.serve_forever()


def main() -> None:
    asked = argparse.ArgumentParser(description=__doc__)
    asked.add_argument("--host", default="0.0.0.0")
    asked.add_argument("--port", type=int, default=5300)
    asked.add_argument("--dir", type=Path, default=Path("/tmp/tetanus-web-chat"))
    args = asked.parse_args()

    if not shutil.which("cargo"):
        raise SystemExit("cargo is not on PATH")
    args.dir.mkdir(parents=True, exist_ok=True)
    served, ws_port = carrier(args.dir)
    try:
        serve(args.host, args.port, ws_port)
    except KeyboardInterrupt:
        print("\nstopped", file=sys.stderr)
    finally:
        served.terminate()


if __name__ == "__main__":
    main()
