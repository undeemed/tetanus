# uiwatch - the terminal UI, live in a browser

The frontend of this project is a terminal, so there is no markup to serve.
`serve.py` serves the real thing instead.

On every change under `crates/`, it rebuilds `tetanus`, runs a fixed set of
scenarios through a pty so the binary sees a terminal and paints exactly as it
would for a user, converts the escape codes to HTML, and pushes a reload to
every open browser.

```
python3 tools/uiwatch/serve.py            # http://localhost:5200
python3 tools/uiwatch/serve.py --port 5300
python3 tools/uiwatch/serve.py --check    # the cell buffer's own test cases
```

It takes the next free port if the one asked for is busy, and prints the URL it
settled on. Editing `serve.py` restarts it in place.

## What it shows, and what it cannot

Each pane is one command, labelled `terminal` or `pipe` - the same binary
answers differently to each, and both answers are worth watching.

An offline turn finishes in milliseconds, so no still image catches the status
line moving. The `status` example (`cargo run -p tetanus-ui --example status`)
drives the same `Progress` renderer slowly, and that pane draws every repaint
on its own line instead of letting each frame overwrite the last.

The live view redraws a block of rows in place, so the cell buffer follows the
cursor up a row and honours both erases as well as `\r`. An ordinary pane
therefore shows what a user would be looking at when the command finished - if
a block leaves anything behind, the pane shows that too, which is the point.
A `repaints` pane opts out of the cursor moves so that every frame stays
visible; that is a deliberate lie about the terminal, and only the two example
panes tell it. `--check` covers both behaviours in six cases.

## Adding a scenario

Append a `Scenario` to `SCENARIOS` in `serve.py`. `setup` runs first and is not
shown, which is how the `replay` pane gets a journal to read.

It builds into `/tmp/tetanus-uiwatch-target`, not `target/`, so a rebuild here
never waits on a `cargo test` you are running yourself.
