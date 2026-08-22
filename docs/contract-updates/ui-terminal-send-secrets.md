# Contract note: a `terminal_send` that types a password is recorded and shown verbatim

Raised by the presentation lane while writing the terminal views
(`web/app/tool-shell.js`). It is a question for whoever owns
[`crates/exec`](../../crates/exec) and the durable vocabulary, not something a
surface can answer, which is why it is a note rather than a change.

## What happens today

`terminal_send` exists so a model can drive a program that will not run without
a real terminal, and the module note names the cases: "a REPL, `ssh`, `git
rebase -i`, anything that asks for a password". The last one is the problem.

A send that answers `[sudo] password for ci:` is an ordinary call:

```json
{ "session_id": "t-1", "text": "hunter2", "submit": true }
```

Its arguments go on the journal like any other tool call, so the credential is
in `sessions/<id>.jsonl` in plain text, permanently, and every surface that
draws a tool call draws it. The web panel does - it puts the text in a code
block with a **Copy** control beside it, which is the most vivid form of the
same fact but not a new one: `tetanus run --ui`, `tetanus chat`, a replay and
anything reading the journal all show it too.

## Why the presentation lane will not fix this

The obvious guess is available and it is wrong. `submit: false` is not a
password signal: both this build and upstream document it as "a control
character (`\u0003` is Ctrl-C) or half a line to a REPL". Masking on it would
hide Ctrl-C and would still show every password sent the ordinary way, with
Enter.

The reliable signal exists, but only on the engine side. The send that carries
a credential is almost always the one immediately after a result that came back
`[wait: stdin_read]` on a prompt the program wrote. A surface can see that
sequence, but acting on it would be a page deciding what is secret by pattern-
matching prose - and getting it wrong in the safe-looking direction, where the
mask makes a reader believe something is protected that is written in full a
directory away.

And redaction on screen alone would be exactly that. The journal is the
durable record; a surface that hid what the journal keeps would be lying about
the risk rather than removing it.

## The three shapes this could take, in the order this lane would prefer them

1. **A `secret: true` on the send**, honoured at record time: the argument is
   replaced by the `<redacted>` sentinel §4.6 already defines before it reaches
   the journal, and the terminal still receives the real text. The model knows
   it is answering a password prompt - it just read the prompt - so it is the
   party that can say so, and the tool's description is where it would be told
   to. Costs one optional field and one branch at the append.

2. **Redaction at record time, decided by the engine**, from the same
   `[wait: stdin_read]` sequence the surface can see. No new field, but the
   decision moves to where the terminal state actually is, and a false negative
   is a leak rather than a cosmetic slip.

3. **Nothing, said out loud.** A documented statement in the terminal tool's
   description and in the operator docs that a terminal session's journal
   contains anything typed into it, so it is to be treated as a credential
   store. This is the honest floor and it is better than the current silence,
   because today nothing anywhere says it.

The presentation lane will draw whichever of these lands. If it is (1) or (2),
the sentinel is already handled: the settings panel draws `<redacted>` as a
withheld value and says in words that it is withheld rather than empty, and the
tool views would do the same.

## What is not being asked for

Not a change to how the text is drawn today. Until the journal stops holding
the credential, drawing it faithfully is the accurate rendering of what
happened, and the **Copy** control is not the leak - it is a control over text
that is already on screen and already on disk.
