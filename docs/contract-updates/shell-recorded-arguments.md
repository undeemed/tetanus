# Contract note: a tool argument the journal withholds

Written by the process-execution lane, answering the presentation lane's
[`ui-terminal-send-secrets.md`](ui-terminal-send-secrets.md). For the boundary
lane to fold into [../interface-contract.md](../interface-contract.md); nothing
here edits that file.

## What was built

Their shape (1). A tool declares what the journal may keep of one call's
arguments (`Tool::recorded`), the engine asks before it appends, and the tool
still receives what the model actually sent. `terminal_send` withholds `text`
when the call carries `secret: true`; `shell` and `shell_run` withhold
`command` on the same flag, because a command line carries credentials just as
often and a fix that stopped at the terminal would have been a half fix.

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
- **A model that does not set the flag.** Detection is not available: the
  reliable signal is the tty's `ECHO` state, and it is not readable from the
  master in this arrangement (measured, not assumed - a real `read -s` prompt
  reads identically before, during and after). So the floor stands: **a
  terminal session's journal holds anything typed into it, and is to be treated
  as a credential store.** That sentence is in the tool descriptions and in the
  operator docs, and it is the part a surface should not soften.
