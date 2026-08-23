# Parity update: a screen for the programs that draw

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file; `docs/parity-updates` was empty on
master before this, so this file is the only copy of what follows.

## 1. The clause this closes

The process-execution row's Gap said it in its own words:

> A screen model for programs that draw with cursor movement - the sanitizer strips escapes and
> keeps no screen, so `htop` and `vim` are runnable here and not readable.

They are readable now. `crates/exec/src/screen.rs` keeps a grid, a cursor, a scrolling region,
insertion and deletion, autowrap, the saved cursor and the alternate screen buffer, fed the same raw
bytes the sanitizer throws away. `terminal_read` hands a model whichever of the two readings is true
for what the session is doing.

## 2. Why two models rather than one

They answer different questions and neither derives from the other:

| | what it answers | right for |
| --- | --- | --- |
| the transcript (`sanitize.rs`) | what did this program *say* | `ls`, `cargo build`, anything that prints and moves on |
| the screen (`screen.rs`) | what would somebody *see* | `htop`, `vim`, `less`, `git rebase -i` |

A drawing program's transcript is every frame concatenated - thousands of lines, of which only the
last screenful is true, with nothing marking which. A printing program has no screen worth reading:
its last forty lines are an arbitrary window on what it said. So both are kept, and the tool picks:
a program on the alternate screen has *announced* that it draws, which is the one reliable signal a
terminal carries, and `as: "scrollback"` is there for a caller that knows better.

Keeping the screen costs `rows * cols` cells - a few kilobytes at 40x160 - which is what makes it
affordable to keep always rather than only when something asks.

## 3. What is modelled, and what is not

Implemented: `CUP`/`CUU`/`CUD`/`CUF`/`CUB`/`CHA`/`VPA`/`CNL`/`CPL`, `ED`, `EL`, `IL`, `DL`, `ICH`,
`DCH`, `ECH`, `DECSTBM` and the scrolling it implies, `DECAWM`, `ESC 7`/`ESC 8` and `CSI s`/`CSI u`,
`ESC M`, and the alternate screen in all three of its spellings.

Not implemented, deliberately: **colour and attributes.** A model reads text; an attribute model
would double the file to record something nothing in this workspace renders. Also absent: character
sets, sixel, mouse reporting, and answering device queries - nothing this crate runs has needed a
reply, and a terminal that answers a query it does not model would be lying in a new way.

## 4. Two defects it found, and how

Both were found by pointing the cases at the real `vim` and the real `htop` instead of at `printf`
fixtures, and neither was visible to any case that used escape sequences an author had typed:

- **Terminal sessions advertised `TERM=dumb`.** Correct for a pipe, exactly wrong here: it tells a
  program there is no screen, so `vim` refuses to draw and `htop` exits. The family this layer
  exists for had been degraded to what a pipe already gave.
- **A session had no `HOME` and no `PATH`.** Nothing is inherited in this crate - right for a
  one-shot command, wrong for an interactive program. No `HOME` is a `git` with no configuration, an
  `ssh` with no keys, and a `vim` that paints its status line and stops. It read as a hole in the
  screen model for an hour. `TerminalConfig::passed` is the fix and it is the shape this lane
  already uses for hooks: a list of names that pass, never a denylist of names that do not.

## 5. Section 3 row

**Today** gains:

> A screen for the programs that draw: a grid, a cursor, a scrolling region and the alternate screen
> buffer, kept beside the transcript and fed the same bytes, so `terminal_read` answers with what a
> reader would see when a program is drawing and with the transcript when it is printing. Terminal
> sessions tell a program it has a real terminal, and carry the named variables an interactive
> program cannot work without.

**Gap** loses the screen-model clause entirely. What remains in that row: `run_in_background` for
one-shot commands, waiting on the job store; a prompt marker for PowerShell; a Windows host; and the
credential prompts the backstop does not recognise.

## 6. Section 4 row

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| — (upstream renders a live terminal in its web card with `xterm.js`, which is a screen model on the *presentation* side; there is no engine-side counterpart to port) | `crates/exec/tests/upstream_screen.rs`, `crates/exec/tests/upstream_terminal_tools.rs` | What a terminal is showing | TC-PORT-SCREEN-1..7, TC-PORT-TERM-45. Five cases feed the grid sequences directly, because a case driving a real program cannot say which sequence it is asserting; two drive the real `vim` and the real `htop`, because a model of a screen that only meets sequences its author thought of is a model of that author's expectations |

## 7. Where this leaves the boundary

Nothing here reaches the engine/presentation contract, and that is deliberate: a surface drawing a
live terminal wants a stream of frames, which is a vocabulary the contract does not have and this
lane is not inventing. What exists now is the engine-side model such a vocabulary would be built
from, and `TerminalSession::screen`, `::cursor` and `::is_drawing` are the three things it would
carry.
