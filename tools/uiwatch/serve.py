#!/usr/bin/env python3
"""A live preview of the tetanus terminal UI, in a browser.

The frontend of this project is a terminal, so there is nothing to serve as
markup. This serves the real thing instead: it rebuilds the `tetanus` binary
whenever a source file changes, runs a fixed set of scenarios through a pty so
the binary sees a terminal and paints exactly as it would for a user, converts
the emitted escape codes to HTML, and pushes a reload to any open browser.

What a reviewer sees is therefore the binary's own output, not a mock of it.

Usage: python3 tools/uiwatch/serve.py [--host 0.0.0.0] [--port 5200]
"""

from __future__ import annotations

import argparse
import html
import os
import pty
import re
import select
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from dataclasses import dataclass, field
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TARGET = Path(os.environ.get("UIWATCH_TARGET", "/tmp/tetanus-uiwatch-target"))
BIN = TARGET / "debug" / "tetanus"
EXAMPLES = TARGET / "debug" / "examples"
COLUMNS = "88"
POLL_SECONDS = 0.4
#: How often the event stream looks for something to say. It says nothing when
#: there is nothing, so this is only how late a browser hears, not how much it
#: is sent.
TICK_SECONDS = 0.25
#: The address a reviewer opens. The server binds every interface, so the line
#: it prints has to name the one they can reach: `localhost` is only true on
#: the machine the server runs on, and this is not read there.
PUBLIC_HOST = "15.204.113.4"
#: The hosts that mean "every interface". Bound to one of these, the server has
#: no one address of its own to name, so it names the public one instead.
WILDCARD = ("0.0.0.0", "::", "")

# The palette the page paints ANSI colours in. Only the eight base colours plus
# bold and dim are used by `tetanus-ui`; clap adds underline.
PALETTE = {
    30: "#5c6370", 31: "#e06c75", 32: "#98c379", 33: "#e5c07b",
    34: "#61afef", 35: "#c678dd", 36: "#56b6c2", 37: "#dcdfe4",
    90: "#7f848e", 91: "#ff7b86", 92: "#b3e88f", 93: "#ffd68a",
    94: "#82c4ff", 95: "#dd9ff0", 96: "#6fd6e0", 97: "#ffffff",
}


@dataclass
class Scenario:
    """One command to show. `setup` runs first and is not displayed."""

    title: str
    why: str
    argv: list[str]
    setup: list[list[str]] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    tty: bool = True
    #: Show each repaint on its own line instead of letting it overwrite the
    #: last. A spinner is invisible in a still image otherwise.
    repaints: bool = False
    #: A program other than the `tetanus` binary, by name, under the examples
    #: directory. Used for the parts of the UI a whole turn is too fast to show.
    example: str | None = None
    #: Keystrokes to send once the view is up, as (bytes, seconds to wait
    #: after). A full-screen view that reads keys paints nothing worth showing
    #: until somebody types at it, and it never exits on its own.
    keys: list[tuple[bytes, float]] = field(default_factory=list)


SCENARIOS = [
    Scenario("tetanus run", "the default view: the turn, read back from the journal",
             ["run", "-p", "hello there", "--session", "j.jsonl"]),
    Scenario("tetanus run --trace --verbose", "the debugging view: every extension point, with payloads",
             ["run", "-p", "hello there", "--trace", "--verbose", "--session", "t.jsonl"]),
    Scenario("tetanus --help", "root help: commands, the colour flag, examples, environment",
             ["--help"]),
    Scenario("tetanus run --help", "one subcommand's page, with its own examples block",
             ["run", "--help"]),
    Scenario("tetanus replay j.jsonl", "the same renderer, on a journal from a previous run",
             ["replay", "j.jsonl"],
             setup=[["run", "-p", "echo this", "--session", "j.jsonl"]]),
    Scenario("tetanus replay j.jsonl --live", "the same journal played back: the block redraws, then leaves nothing",
             ["replay", "j.jsonl", "--live", "--speed", "6"],
             setup=[["run", "-p", "echo this", "--session", "j.jsonl"]]),
    Scenario("tetanus chat --ui", "the conversation on a screen of its own: a turn asked, the transcript kept, the prompt pinned",
             ["chat", "--ui", "-a", "mock", "--session", "c.jsonl"],
             keys=[(b"what does a turn look like\r", 3.0), (b"/keys\r", 1.0), (b"\x04", 0.6)]),
    Scenario("tetanus config", "every resolved key, and the layer that set it", ["config"]),
    Scenario("tetanus info", "what this build is", ["info"]),
    Scenario("tetanus run --adapter deepseek", "the failure surface: error, then the way out",
             ["run", "--adapter", "deepseek", "--session", "never.jsonl"]),
    Scenario("cargo run --example status", "the status line slowed down: every repaint, one frame per line",
             [], example="status", repaints=True),
    Scenario("cargo run --example screen", "the live block slowed down: one frame per row, then what it commits",
             [], example="screen", repaints=True),
    Scenario("tetanus run | cat", "the same run into a pipe: no escape codes, ever",
             ["run", "-p", "hello there", "--session", "p.jsonl"], tty=False),
]


class Screen:
    """A cell buffer, so `\\r` overwrites the way a terminal would.

    It also follows the cursor up a row and erases, because `tetanus-ui`'s
    `Screen` redraws a block in place. A pane that ignored those escapes would
    stack every frame and claim the product had printed all of them.
    """

    def __init__(self, repaints: bool = False) -> None:
        self.repaints = repaints
        self.lines: list[list[tuple[str, tuple]]] = [[]]
        #: The row the cursor is on. Only a redrawn block ever leaves the last.
        self.row = 0
        self.column = 0
        self.style = (None, False, False, False)

    def sgr(self, params: list[int]) -> None:
        fg, bold, dim, underline = self.style
        for code in params or [0]:
            if code == 0:
                fg, bold, dim, underline = None, False, False, False
            elif code == 1:
                bold = True
            elif code == 2:
                dim = True
            elif code == 4:
                underline = True
            elif code in (22, 21):
                bold = dim = False
            elif code == 24:
                underline = False
            elif code == 39:
                fg = None
            elif code in PALETTE:
                fg = code
        self.style = (fg, bold, dim, underline)

    def write(self, text: str) -> None:
        i = 0
        while i < len(text):
            char = text[i]
            if char == "\x1b":
                match = re.match(r"\x1b\[([0-9;]*)m", text[i:])
                if match:
                    raw = match.group(1)
                    self.sgr([int(p) for p in raw.split(";") if p != ""] if raw else [0])
                    i += match.end()
                    continue
                move = re.match(r"\x1b\[([0-9]*)([AJK])", text[i:])
                if move:
                    self.control(move.group(2), move.group(1))
                    i += move.end()
                    continue
                # `ESC [ H`, and `ESC [ row ; col H`: a full-screen view homes
                # the cursor to start a frame, and places it on the row it
                # wants a caret on before it finishes one.
                home = re.match(r"\x1b\[([0-9]*)(?:;([0-9]*))?H", text[i:])
                if home:
                    self.place(home.group(1), home.group(2))
                    i += home.end()
                    continue
                skip = re.match(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07", text[i:])
                i += skip.end() if skip else 1
                continue
            if char == "\n":
                self.down()
                self.column = 0
            elif char == "\r":
                if text[i + 1:i + 2] == "\n":
                    i += 1  # a pty writes CRLF for a plain newline
                    continue
                if self.repaints and self.lines[self.row]:
                    # An erase - the blanking pass between two frames - is not
                    # a frame, so it gets overwritten rather than a line.
                    if any(char != " " for char, _ in self.lines[self.row]):
                        self.down()
                    else:
                        self.lines[self.row] = []
                self.column = 0
            elif char == "\t":
                self.column += 4 - (self.column % 4)
            elif char >= " ":
                line = self.lines[self.row]
                while len(line) <= self.column:
                    line.append((" ", (None, False, False, False)))
                line[self.column] = (char, self.style)
                self.column += 1
            i += 1

    def place(self, row: str, column: str) -> None:
        """Put the cursor where an absolute move asks for it.

        A full-screen view paints by homing and writing every row, so without
        this each frame lands under the one before it and the pane shows a
        conversation repeated once per repaint. A repaints pane is stacking on
        purpose and is left alone.
        """
        if self.repaints:
            return
        self.row = max(0, int(row) - 1) if row else 0
        self.column = max(0, int(column) - 1) if column else 0
        while len(self.lines) <= self.row:
            self.lines.append([])

    def control(self, final: str, raw: str) -> None:
        """The three escapes the product writes: cursor up, and the two erases.

        Anything else stays skipped, as it was before this understood any of
        them. A pane that guessed at an escape it had never seen would be a
        worse lie than a pane that dropped it.
        """
        count = int(raw) if raw else (0 if final in "JK" else 1)
        if final == "A":
            # A repaints pane is deliberately stacking every frame, so a frame
            # must not be allowed to move back over the one before it.
            if not self.repaints:
                self.row = max(0, self.row - count)
        elif final == "K" and count in (0, 2):
            line = self.lines[self.row]
            del line[self.column if count == 0 else 0:]
        elif final == "J" and count in (0, 2):
            if count == 2:
                self.lines, self.row, self.column = [[]], 0, 0
                return
            del self.lines[self.row][self.column:]
            del self.lines[self.row + 1:]

    def down(self) -> None:
        """One row down, onto the row already there if the cursor came up."""
        self.row += 1
        while len(self.lines) <= self.row:
            self.lines.append([])

    def to_html(self) -> str:
        out = []
        for line in self.lines:
            while line and line[-1][0] == " ":
                line.pop()
            parts, run, style = [], [], None
            for char, cell_style in line:
                if cell_style != style and run:
                    parts.append(span(style, "".join(run)))
                    run = []
                style = cell_style
                run.append(char)
            if run:
                parts.append(span(style, "".join(run)))
            out.append("".join(parts))
        while out and out[-1] == "":
            out.pop()
        return "\n".join(out)


def span(style: tuple | None, text: str) -> str:
    escaped = html.escape(text)
    if not style:
        return escaped
    fg, bold, dim, underline = style
    css = []
    if fg is not None:
        css.append(f"color:{PALETTE[fg]}")
    if bold:
        css.append("font-weight:700")
    if dim:
        css.append("opacity:.62")
    if underline:
        css.append("text-decoration:underline")
    return f'<span style="{";".join(css)}">{escaped}</span>' if css else escaped


def env_for(extra: dict[str, str]) -> dict[str, str]:
    env = dict(os.environ)
    for name in ("NO_COLOR", "CLICOLOR", "DEEPSEEK_API_KEY"):
        env.pop(name, None)
    env.update({"TERM": "xterm-256color", "COLUMNS": COLUMNS, "CLICOLOR_FORCE": "1"})
    env.update(extra)
    return env


def capture(argv: list[str], cwd: Path, env: dict[str, str], tty: bool,
            program: Path = BIN,
            keys: list[tuple[bytes, float]] | None = None) -> tuple[str, int]:
    """Run a program and return everything it painted, plus its exit status.

    `keys` is for the views that read them: each pair is what to type and how
    long to let the view answer before the next. A view that reads keys is also
    a view that never ends on its own, so the last pair is whatever leaves it -
    ctrl-D for the chat - and the wait after it is the one that catches the
    page it leaves behind.
    """
    if not tty:
        done = subprocess.run([str(program), *argv], cwd=cwd, env=env,
                              capture_output=True, timeout=60)
        # A pipe keeps the two streams apart, so the page should too.
        text = done.stdout.decode(errors="replace")
        if done.stderr:
            text += "\x1b[2m--- stderr ---\x1b[0m\n" + done.stderr.decode(errors="replace")
        return text, done.returncode

    leader, follower = pty.openpty()
    # A view that reads keys needs the terminal on its standard input as well,
    # and one that does not is unchanged by being given it: neither the help
    # page nor a turn reads a keystroke.
    stdin = follower if keys else subprocess.DEVNULL
    proc = subprocess.Popen([str(program), *argv], cwd=cwd, env=env,
                            stdin=stdin, stdout=follower, stderr=follower)
    os.close(follower)
    chunks: list[bytes] = []

    def drain(seconds: float) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([leader], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(leader, 65536)
            except OSError:
                return
            if data:
                chunks.append(data)

    if keys:
        # The view has to be up before it is typed at, or the keys land in the
        # terminal's buffer and arrive all at once as a paste.
        drain(1.2)
        for typed, wait in keys:
            try:
                os.write(leader, typed)
            except OSError:
                break
            drain(wait)

    while True:
        ready, _, _ = select.select([leader], [], [], 0.2)
        if ready:
            try:
                data = os.read(leader, 65536)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
        elif proc.poll() is not None:
            break
    os.close(leader)
    return b"".join(chunks).decode(errors="replace"), proc.wait()


#: What a terminal writes when a view takes the alternate screen, and gives it
#: back. Everything between the two is the view; everything outside it is the
#: shell's own screen, which the view is careful not to disturb.
ALTERNATE = ("\x1b[?1049h", "\x1b[?1049l")


def on_the_alternate_screen(text: str) -> str:
    """What a full-screen view painted, without the page it was opened from.

    A view on the alternate screen leaves the screen it found when it exits, so
    a pane rendered from the whole capture ends up showing the shell's page
    with the view's frames stacked above it. What a reviewer wants to see is
    the view: the frames between the two switches, of which the last is the
    one that was on the terminal when they left it.
    """
    if ALTERNATE[0] not in text:
        return text
    view = text.split(ALTERNATE[0], 1)[1]
    return view.rsplit(ALTERNATE[1], 1)[0] if ALTERNATE[1] in view else view


def render_scenarios() -> list[dict]:
    panes = []
    for scenario in SCENARIOS:
        with tempfile.TemporaryDirectory() as workdir:
            cwd, env = Path(workdir), env_for(scenario.env)
            for argv in scenario.setup:
                capture(argv, cwd, env, tty=False)
            program = EXAMPLES / scenario.example if scenario.example else BIN
            try:
                text, status = capture(
                    scenario.argv, cwd, env, scenario.tty, program, scenario.keys
                )
            except subprocess.TimeoutExpired:
                text, status = "the command did not finish inside 60s", -1
        text = on_the_alternate_screen(text)
        screen = Screen(scenario.repaints)
        screen.write(text)
        panes.append({
            "title": scenario.title, "why": scenario.why,
            "body": screen.to_html(), "status": status, "tty": scenario.tty,
        })
    return panes


def sources() -> dict[str, object]:
    """What a rebuild is keyed on: the crates, and the commit they sit on.

    The mtimes are not enough on their own. `git commit` writes nothing in the
    working tree, and a checkout between two branches that differ only outside
    `crates/` writes nothing either, so a page that watched the files alone
    would go on naming the previous branch and the previous commit in its
    header while the panes under it are the current build. That header is the
    only provenance a reviewer has for what they are looking at, so it is not
    allowed to disagree with the panes: the revision is part of what a change
    means here.
    """
    stamps: dict[str, object] = {"HEAD": revision()}
    for path in (ROOT / "crates").rglob("*"):
        if path.suffix in (".rs", ".toml") and ".git" not in path.parts:
            try:
                stamps[str(path)] = path.stat().st_mtime
            except OSError:
                pass
    return stamps


class State:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.version = 0
        self.building = False
        self.page = "<p>building…</p>"

    def begin(self) -> None:
        """Cargo has started, so the page being served is the previous one."""
        with self.lock:
            self.building = True

    def publish(self, page: str) -> None:
        with self.lock:
            self.version += 1
            self.building = False
            self.page = page

    def read(self) -> tuple[int, str]:
        with self.lock:
            return self.version, self.page

    def signal(self) -> str:
        """What the event stream carries: the version, and whether cargo runs.

        One string, so a browser that is up to date and a browser that is a
        build behind are told apart by comparing it, and so the stream has
        something to be quiet about when neither has moved.
        """
        with self.lock:
            return f"{self.version} {int(self.building)}"


STATE = State()


def git(*args: str) -> str:
    try:
        return subprocess.run(["git", *args], cwd=ROOT, capture_output=True,
                              text=True, timeout=10).stdout.strip()
    except Exception:
        return "?"


def revision() -> tuple[str, str]:
    """The branch and the commit a page is built from.

    One reader of this is `sources`, which keys a rebuild on it, and the other
    is `page`, which draws it in the header. Reading it in one place is what
    stops those two from disagreeing: whatever moves the header is by the same
    fact a change, so the build the header describes is the build under it.

    A git call that fails answers `?`, which counts as a change and costs one
    rebuild it did not need. That is the cheaper way round: a page nobody can
    trust the provenance of is worse than a page that was drawn twice.
    """
    return git("branch", "--show-current"), git("log", "--oneline", "-1")


def build_and_render() -> None:
    STATE.begin()
    started = time.monotonic()
    build = subprocess.run(
        ["cargo", "build", "--quiet", "-p", "tetanus-hardness", "--bin", "tetanus",
         "-p", "tetanus-ui", "--example", "status",
         "-p", "tetanus-ui", "--example", "screen"],
        cwd=ROOT, capture_output=True, text=True,
        env={**os.environ, "CARGO_TARGET_DIR": str(TARGET)})
    seconds = time.monotonic() - started
    if build.returncode != 0:
        screen = Screen()
        screen.write(build.stderr or build.stdout)
        STATE.publish(page(None, screen.to_html(), seconds))
        return
    STATE.publish(page(render_scenarios(), None, seconds))


def page(panes: list[dict] | None, failure: str | None, seconds: float) -> str:
    stamp = datetime.now().strftime("%H:%M:%S")
    branch, commit = revision()
    if failure is not None:
        body = (f'<section class="pane broken"><header><h2>the build failed</h2>'
                f'<p>nothing below is current</p></header><pre>{failure}</pre></section>')
    else:
        body = "".join(
            f'<section class="pane"><header><h2>{html.escape(pane["title"])}</h2>'
            f'<p>{html.escape(pane["why"])}</p>'
            f'<span class="tag">{"terminal" if pane["tty"] else "pipe"}</span>'
            f'<span class="tag{" bad" if pane["status"] else ""}">exit {pane["status"]}</span>'
            f'</header><pre>{pane["body"]}</pre></section>'
            for pane in panes or [])
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>tetanus ui - live</title>
<style>
 :root {{ color-scheme: dark; }}
 * {{ box-sizing: border-box; min-width: 0; }}
 body {{ margin:0; background:#14161c; color:#dcdfe4;
   font:15px/1.55 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif; }}
 header.top {{ position:sticky; top:0; z-index:2; padding:14px 22px;
   background:#0f1116ee; backdrop-filter:blur(8px); border-bottom:1px solid #262a33;
   display:flex; gap:16px; align-items:baseline; flex-wrap:wrap; }}
 header.top h1 {{ font-size:15px; margin:0; letter-spacing:.02em; }}
 .meta {{ color:#7f848e; font-size:12.5px; font-family:ui-monospace,monospace; }}
 .live {{ color:#98c379; font-size:12.5px; transition:color 200ms ease; }}
 .live .dot {{ display:inline-block; width:7px; height:7px; border-radius:50%;
   background:currentColor; margin-right:6px; vertical-align:middle; }}
 /* Amber and breathing while cargo runs. The panes under this are the
    previous build for as long as that takes, up to ten seconds on a cold
    one, and a still page under a green light claims to be current when it is
    not. The dot moves rather than only changing colour because a state that
    lasts seconds and never moves reads as stuck: the word says what is
    happening, the movement says it is still happening. Anyone who has asked
    not to be moved gets the colour and the word on their own. */
 .live.building {{ color:#e5c07b; }}
 .live.building .dot {{ animation:breathe 1.4s ease-in-out infinite alternate; }}
 @keyframes breathe {{ from {{ opacity:1; }} to {{ opacity:.3; }} }}
 @media (prefers-reduced-motion: reduce) {{
   .live.building .dot {{ animation:none; opacity:.6; }} }}
 /* Columns, not a grid: a card is as tall as its output, and the next card
    starts under it rather than under the tallest card in its row. */
 main {{ padding:22px; column-width:760px; column-gap:18px; }}
 .pane {{ background:#1a1d24; border:1px solid #262a33; border-radius:10px;
   overflow:hidden; break-inside:avoid; margin:0 0 18px; }}
 .pane header {{ padding:11px 15px; border-bottom:1px solid #262a33;
   display:flex; gap:10px; align-items:baseline; flex-wrap:wrap; }}
 .pane h2 {{ font:600 13.5px ui-monospace,monospace; margin:0; color:#e5c07b; }}
 .pane header p {{ margin:0; color:#7f848e; font-size:12.5px; flex:1 1 220px; }}
 .tag {{ font:11px ui-monospace,monospace; color:#7f848e;
   border:1px solid #323742; border-radius:99px; padding:1px 8px; white-space:nowrap; }}
 .tag.bad {{ color:#e06c75; border-color:#5c2c31; }}
 .broken h2 {{ color:#e06c75; }}
 pre {{ margin:0; padding:15px; overflow-x:auto; white-space:pre; tab-size:4;
   font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace; }}
</style></head><body>
<header class="top"><h1>tetanus ui</h1>
 <span class="meta">{html.escape(branch)} · {html.escape(commit)}</span>
 <span class="meta">built in {seconds:.1f}s · {stamp}</span>
 <span class="live"><span class="dot"></span><span class="what">live</span></span></header>
<main>{body}</main>
<script>
 const here = {STATE.version + 1};
 const live = document.querySelector('.live'), what = live.querySelector('.what');
 new EventSource('/events').onmessage = e => {{
   const [version, building] = e.data.split(' ');
   if (+version !== here) {{ location.reload(); return; }}
   live.classList.toggle('building', building === '1');
   what.textContent = building === '1' ? 'building' : 'live';
 }};
</script></body></html>"""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args) -> None:  # quiet; the log is the build output
        pass

    def do_GET(self) -> None:
        if self.path.startswith("/events"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            # Only on change. Nothing here moves between builds, and a
            # stream that repeated itself once a second would keep every open
            # browser awake for as long as it was left open. The comment is
            # not an event: it is there so anything that closes an idle
            # connection sees the connection is not idle.
            said, quiet = None, 0.0
            try:
                while True:
                    signal = STATE.signal()
                    if signal != said:
                        self.wfile.write(f"retry: 1000\ndata: {signal}\n\n".encode())
                        said, quiet = signal, 0.0
                    elif quiet >= 15.0:
                        self.wfile.write(b": still here\n\n")
                        quiet = 0.0
                    self.wfile.flush()
                    time.sleep(TICK_SECONDS)
                    quiet += TICK_SECONDS
            except (BrokenPipeError, ConnectionResetError):
                return
        _, body = STATE.read()
        payload = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def own_stamp(last: float) -> float:
    """This file's timestamp, or the last one seen.

    A `git checkout` takes the file away for a few milliseconds. Losing the
    watcher to that race leaves the server up and the page frozen for good,
    which is worse than one poll on a stale timestamp.
    """
    try:
        return Path(__file__).stat().st_mtime
    except OSError:
        return last


def watch() -> None:
    try:
        poll()
    except Exception:
        traceback.print_exc()
        # keep.sh restarts a dead process, but it cannot see a dead thread,
        # and a server with no watcher serves one page for ever.
        os._exit(1)


def poll() -> None:
    build_and_render()
    seen, own = sources(), own_stamp(0.0)
    while True:
        time.sleep(POLL_SECONDS)
        if own_stamp(own) != own:
            # This file is part of the UI lane too. Reload it in place rather
            # than serving a page whose renderer is out of date.
            print("uiwatch: serve.py changed, restarting", flush=True)
            os.execv(sys.executable, [sys.executable, *sys.argv])
        now = sources()
        if now == seen:
            continue
        time.sleep(0.35)  # let an editor finish writing the rest of its files
        seen = sources()
        build_and_render()


def rows(text: str, repaints: bool = False) -> list[str]:
    """The buffer as plain rows, for the self-check below."""
    screen = Screen(repaints)
    screen.write(text)
    return ["".join(char for char, _ in line) for line in screen.lines]


def check() -> None:
    """Test cases for the cell buffer. Run with `--check`.

    The buffer is the only part of this tool that can be wrong quietly: a
    dropped escape does not crash anything, it just makes the page claim the
    product printed something it never printed.
    """
    # TC-WATCH-1: a carriage return overwrites its own row.
    assert rows("frame A\rframe B") == ["frame B"]
    # TC-WATCH-2: a cursor up replaces the frame that is already on the row.
    assert rows("keep\r\nold\r\n\x1b[1A\rnew\r\n") == ["keep", "new", ""]
    # TC-WATCH-3: erase to end of line takes the tail of a longer frame.
    assert rows("longer frame\r\n\x1b[1A\rshort\x1b[K\r\n") == ["short", ""]
    # TC-WATCH-4: erase to end of display takes the rows under the cursor.
    assert rows("a\r\nb\r\nc\r\n\x1b[2A\r\x1b[J") == ["a", ""]
    # TC-WATCH-5: a repaints pane keeps every frame, cursor up or not.
    assert rows("one\r\n\x1b[1A\rtwo\r\n", repaints=True) == ["one", "two", ""]
    # TC-WATCH-6: an escape this does not implement is dropped, not printed.
    assert rows("\x1b[?25lhidden\x1b[?25h") == ["hidden"]
    # TC-WATCH-7: a full-screen view homes the cursor to start a frame, so the
    # second frame overwrites the first rather than landing under it.
    assert rows("\x1b[Hone\r\ntwo\x1b[Hnew\r\n") == ["new", "two"]
    # TC-WATCH-8: an absolute move puts the cursor on a row and a column, which
    # is how a view that is typed into says where its caret goes.
    assert rows("\x1b[Ha\r\nb\r\nc\x1b[2;2Hx") == ["a", "bx", "c"]
    # TC-WATCH-9: what a view painted is what it painted on the alternate
    # screen - the page it was opened from is not part of the view.
    assert on_the_alternate_screen("before\x1b[?1049hinside\x1b[?1049lafter") == "inside"
    assert on_the_alternate_screen("no alternate screen here") == "no alternate screen here"
    # A view killed before it left keeps everything it painted.
    assert on_the_alternate_screen("shell\x1b[?1049hview") == "view"
    print("uiwatch: the cell buffer agrees with all 9 cases")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="0.0.0.0",
                        help="the address to bind (default: every interface)")
    parser.add_argument("--port", type=int, default=5200)
    parser.add_argument("--tries", type=int, default=12)
    parser.add_argument("--check", action="store_true",
                        help="run the cell buffer's self-check and exit")
    args = parser.parse_args()

    if args.check:
        check()
        return

    if not shutil.which("cargo"):
        raise SystemExit("cargo is not on PATH")

    for port in range(args.port, args.port + args.tries):
        try:
            server = ThreadingHTTPServer((args.host, port), Handler)
        except OSError:
            continue
        server.daemon_threads = True
        shown = PUBLIC_HOST if args.host in WILDCARD else args.host
        print(f"uiwatch: http://{shown}:{port} (worktree {ROOT})", flush=True)
        threading.Thread(target=watch, daemon=True).start()
        server.serve_forever()
        return
    raise SystemExit(f"no free port in {args.port}..{args.port + args.tries - 1}")


if __name__ == "__main__":
    main()
