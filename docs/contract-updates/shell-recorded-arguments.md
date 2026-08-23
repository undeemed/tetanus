# Contract note: a tool argument the journal withholds

Written by the process-execution lane, answering the presentation lane's
[`ui-terminal-send-secrets.md`](ui-terminal-send-secrets.md). For the boundary
lane to fold into [../interface-contract.md](../interface-contract.md); nothing
here edits that file.

## What was built

The hybrid the captain decided from the scout's report
(`data/tetanus-secrets-scout/report.md`): a caller flag and a runtime backstop,
composed by union, plus the documentation half.

**The flag.** A tool declares what the journal may keep of one call's arguments
(`Tool::recorded`), the engine asks before it appends, and the tool still
receives what the model actually sent. `terminal_send` withholds `text` when
the call carries `secret: true`; `shell` and `shell_run` withhold `command` on
the same flag, because a command line carries credentials just as often and a
fix that stopped at the terminal would have been a half fix.

**The backstop.** A terminal arms a window when a program's last output line
looks like a credential prompt, and a send into an armed terminal is recorded
withheld whether or not the model set the flag. The mechanism is `sudo`'s,
shipped in 1.9.10 for this exact problem; the rule
(`tetanus_turn::tools::looks_like_a_password_prompt`) is one answer for the
whole harness, and the evidence stays with the terminal because nothing else
has it.

**Union, never override**, which is the direction §4.3 already fixes for the
two config-redaction rules: either says secret, it is secret; neither can
un-say it. A rule that could un-redact would make adding one a way to start
publishing, silently and permanently.

Two mechanisms were considered and are **not** built, both measured dead rather
than argued away: inferring from the `[wait: stdin_read]` sequence, whose
premise is inverted here - that is the *ordinary* settle reason for every
command that finishes, while a program stopping to ask for a password emits no
prompt marker at all - and `ECHO`-off detection, which readline and this
crate's own `stty -echo` both pin to a constant.

## A citation correction

The note that raised this cited the sentinel as §4.6. It is **§4.3** (lines
340-364); §4.6 is "State dynamics". Everything below cites §4.3.

## What the contract has to say, and why it is not just "reuse §4.3's sentinel"

The withheld value is `types::REDACTED`, the same `<redacted>` string §4.3
publishes for a withheld configuration value, because a surface that already
draws one as *withheld, not empty* should not need a second vocabulary.

But §4.3 carries a warning that does not survive the move unchanged:

> Nothing distinguishes a withheld value from a document that literally
> contains the string `<redacted>`, and a surface that treated the sentinel as
> "this is a secret" would mislabel the second.

For a **configuration value** that is right: the document is the user's, and it
can contain anything. For a **tool argument** it is weaker than what is now
true, and the difference is worth writing down rather than leaving each surface
to guess:

- The sentinel in a `tool/call`, in an `assistant/message`'s `tool_calls`, or in
  an `assistant/chunk`'s streamed call is **minted by the engine**. It appears
  because a tool asked for it.
- A model *can* send the literal string `<redacted>` as an argument, and then
  the journal holds it for the ordinary reason. So the sentinel still is not
  proof - but the ambiguity is now between "the engine withheld this" and "the
  model typed the sentinel", which is a much narrower confusion than §4.3's
  and one no surface behaviour turns on.

The honest resolution is the one §4.3 already names as deferred: a flag on the
record rather than a magic string. That is a type change on the boundary, so by
§5 it lands as its own PR touching both `crates/protocol` and the contract, and
this note is not it. Until then a surface should draw the sentinel in a tool
argument the way it draws it in a config value - withheld, not empty - and say
so in words.

## The three places it appears

Worth listing, because the first is the obvious one and the other two are where
the leak actually survived a first attempt at fixing it:

1. `tool/call` - the record of the call the engine dispatched.
2. `assistant/message` - the model *said* the credential, so the message that
   carried the call holds the same arguments.
3. `assistant/chunk` - the streamed half of the same thing, which a replay and
   a live surface both read.

A consumer that reconstructs a request from the journal therefore sees the
sentinel in the model's own prior call. That is intended: the model does not
need to re-read a password it already sent, and the alternative is the
credential in the record.

## What this does not cover, and what a surface should still assume

- **A `tool/result`.** What a program prints is bounded by the terminal's own
  echo behaviour, not by this flag. Our terminals run with echo off, so a
  password typed at `sudo` does not come back in the viewport - but a program
  that echoes what it is given (a REPL taking a token) puts it in the result,
  and nothing withholds it.
- **A model that does not set the flag, at a prompt the backstop does not
  recognise.** The window arms on `password` and `passphrase` wording; a
  program that asks for a "PIN", a "one-time code" or nothing at all leaves it
  closed. The floor therefore stands whatever the mechanisms catch: **a
  terminal session's journal holds anything typed into it, and is to be treated
  as a credential store.** That sentence is in the tool descriptions and in the
  operator docs, and it is the part a surface should not soften.
