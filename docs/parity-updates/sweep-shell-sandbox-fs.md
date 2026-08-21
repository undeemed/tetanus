# Sweep: what is left in the `shell/*`, `terminal/*`, `subprocess/*`, `sandbox/*` and `fs/*` rows

A sweep rather than a port. Every gap clause those five rows carried was read against the code that
now exists, and each one is recorded below as served, genuinely still open with the reason, or -
in one case - closed by this slice because it was the last piece that could be built with what the
workspace already had.

Three rows in [../parity.md](../parity.md) are rewritten in place by this slice, and nothing else in
that file is touched: `shell/*, terminal/*, subprocess/*`, `sandbox/*`, and the sandbox clause of
`fs/*`. The section 4 rows for the ported suites are already written verbatim in
[shell-process-execution.md](shell-process-execution.md),
[sandbox-policy-and-landlock.md](sandbox-policy-and-landlock.md) and the fs lane's
[fs-service.md](fs-service.md), so they are not duplicated here.

## 1. What the sweep closed

**The per-call escalation stamp** (`sandbox/*`), which both earlier notes named as the one remaining
follow-up that was genuinely buildable. Upstream's `escalation.spec.ts` is an approval protocol over
a policy: a denied command may be retried once under a wider mode, with the person running the
harness consenting. Both halves already existed here - `Policy::widened_to` is one axis widened, and
`tetanus_turn::approval` is a decision with a durable asked/decided pair - so the escalation is the
two of them meeting rather than a new mechanism:

- The `shell` tool declares `Permission::Ask` for a call carrying `sandbox_permissions`, so the
  question goes through the engine's existing gate and is audited like every other decision. The
  reason names the command, both modes and the model's own sentence.
- The grant applies to that one call: a wider executor is built for it and dropped, because a cached
  wider executor is a grant that outlives the question that bought it.
- A malformed request - unpaired arguments, an unknown mode, or one no wider than the mode already
  in force - is refused in words and **no question is put**, because an unanswerable question is
  worse for the person answering than a refusal.
- The two arguments are advertised only where there is a wider mode to ask for.

TC-PORT-SANDBOX-28..31. Writing them caught one case passing for the wrong reason: the refusal case
first asked for `workspace-full-access`, which is not a mode, so it was refused by validation rather
than by the answerer. It now asserts the audit pair to prove which of the two refused.

## 2. What is genuinely still open, and why

Named so the rows can be read as the truth rather than as a to-do list nobody triaged.

### `shell/*`, `terminal/*`, `subprocess/*`

| Still open | Why it is not closed here |
| --- | --- |
| A PTY, and the interactive behaviour that needs one: viewport, scrollback paging, terminal size, foreground-group signalling, prompt readiness | A real pseudo-terminal is a dependency and a platform surface, and everything above only means something once it exists. The persistent shells serve the behaviour a model actually needs from a session - state that survives, an exit status per command, a bounded transcript - over a pipe, and the marker protocol is *exact* where prompt detection is a guess |
| Background jobs (`run_in_background`, `job_output`, `job_kill`) | One feature with a job store behind it, which is its own row (`workflow/*`, `jobs/*`). Advertising a background mode that cannot be collected would be worse than not advertising one |
| Spill files | A storage policy question - where the file lives, who deletes it, what a confined reader may open - and inventing an answer inside the subprocess seam would settle those by accident. Truncation is reported without a path today |
| Raw piped stdio handed to a protocol consumer | The consumer is MCP-on-stdio or an out-of-process hook, and neither exists yet. Building the seam now would be building it against no caller, which is how a seam ends up the wrong shape |
| Owner-scoped sessions | Waits on a session having an owner identity; today isolation is per registry, and the engine builds one per session |

### `sandbox/*`

| Still open | Why it is not closed here |
| --- | --- |
| A Windows ACL backend | Upstream's is ~1500 lines across FFI, token, ACL, SID and runner. Nothing in this workspace can prove any of it - there is no Windows CI host - and a sandbox nobody has watched deny anything is not a sandbox. The refusal names it, and TC-PORT-SANDBOX-11 pins that the refusal is a refusal |
| A macOS Seatbelt backend | The same argument, the same reason |
| Read confinement | Landlock's allow-list governs effects. Hiding files from a confined process needs a mount namespace or a container, which upstream also treats as a different capability seam |
| A settings key and a CLI flag for the mode | Real and small, but it is configuration surface rather than parity: upstream's own knob is a settings section, and the row it belongs to is `settings/*`. A deployment composes the policy in Rust today |
| Parallel file operations behind the boundary | One confined worker means one file operation at a time. A pool needs one restriction per thread; widening it is a performance slice, not a parity one |

### `fs/*`

Nothing this lane owns is open. The sandbox clause the row carried - "the kernel backends that
isolate untrusted code rather than untrusted paths" - is served. What remains in the row is the fs
lane's own list, unchanged by this sweep: read windows over bytes rather than text, image reads and
the attachment store they need, a search tool over file contents, and diff presentation.

## 3. Rows that are clear

Said plainly, because the instruction for this sweep was to say so rather than invent work:

- **`fs/*` is clear of everything this lane could close.** Its remaining four items belong to the fs
  lane and are already stated in its own note.
- **`sandbox/*` has nothing buildable left on this host.** Every remaining item needs either a
  platform this workspace cannot test on, a container rather than an allow-list, or a settings
  surface that belongs to another row.
- **`shell/*, terminal/*, subprocess/*` has nothing left that is not waiting on something else** -
  a PTY dependency, a job store, a storage policy, or a first consumer. Each remaining clause names
  what it waits on, so the row can be closed by the slice that lands that thing rather than by this
  one guessing at it.
