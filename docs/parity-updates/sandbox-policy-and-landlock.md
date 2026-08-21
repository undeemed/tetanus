# Parity update: `sandbox/*`

Written by the sandboxing lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

This slice is scoped to the policy, the Linux backend, the honest refusal where there is no backend,
and enforcement through `crates/exec`. The filesystem half is named in section 4 as a follow-up
rather than guessed at: it belongs to a crate this branch does not carry.

## 1. Section 3 row

Replace the `sandbox/*` row with:

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `sandbox/*` (policy, local, Windows ACL) | 21 | The policy vocabulary as a value resolved once at a boundary and handed down whole - upstream's three modes (`read-only`, `workspace-write`, `danger-full-access`), the workspace root, the writable roots the mode means including the temp areas a build needs, plus two things upstream leaves to its backends: a network decision, and whether partial enforcement is acceptable. A Linux backend on Landlock, built by hand around the fork/exec split so the child's half between `fork` and `exec` is three allocation-free system calls; deny-by-default is the ABI's own shape, so `mkdir`, `unlink` and `rename` are governed and not only `write`. Enforcement through `crates/exec`: one command, and a persistent shell confined once and inherited by every command it runs. A denial rendered as upstream's denial marker naming the mode, so a model reads policy rather than a bug in its own command; a refused tool call contained as `ok: false` with the reason on the journal and carried into the next request. A host that cannot enforce what was asked refuses at composition - `SandboxError::Unavailable` for a kernel without Landlock, `Degraded` for an ABI that cannot govern the policy, and a compile-time refusal naming the missing backend on a platform that has none | Applying the same policy inside the filesystem service (the crate is another lane's, and the seam is `Policy` + `landlock::confine_current_thread`, both already here); the escalation flow - a denied command retried once under a wider mode with user approval - which needs the policy wired to `tetanus_turn::approval`; a Windows ACL backend; a Seatbelt backend for macOS; read confinement, which needs a container rather than an allow-list; a settings key and a CLI flag, so a deployment can choose a mode without composing in Rust | ② for the fs half and the escalation flow, ③ for the other platforms |

## 2. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `sandbox/sandbox-policy/tests/policy.spec.ts`, `sandbox/sandbox/tests/{roots,vocabulary}.spec.ts` | `crates/sandbox/tests/upstream_sandbox.rs`, `crates/sandbox/src/policy.rs` | The mode vocabulary, and what each mode grants | ported: TC-PORT-SANDBOX-6, -9, plus the module's own three unit cases. The derivation of the writable roots lives in one place for upstream's reason - "the write tool cannot write /tmp but bash can" is an asymmetry that only arises when two layers derive it separately |
| `sandbox/sandbox-local/tests/local.spec.ts`, `probe.spec.ts`, `provider-chain.spec.ts` | `crates/sandbox/tests/upstream_sandbox.rs` | That the kernel refuses, that the host is probed, and that an under-capable host fails closed | ported: TC-PORT-SANDBOX-1..5, -7, -8, -10, -12. Every confinement case restricts a real thread and then performs the operation, so what passes is the kernel's answer; a backend that never made a syscall would fail all of them. Upstream's bwrap and Seatbelt dialects are other hosts' backends and have nothing to restate here |
| `sandbox/sandbox-local` (its runner wrapping an argv), `shell/tool-bash` (its denial marker) | `crates/exec/tests/upstream_sandbox_exec.rs` | The boundary applied to real commands, and what the model is told | ported: TC-PORT-SANDBOX-13..19. Upstream wraps an argv in a runner process; this applies the ruleset in the child between `fork` and `exec`, which is the same boundary without a second process, and TC-PORT-SANDBOX-13 pins the property that matters either way - the child is confined and the parent is not |
| `sandbox/sandbox-windows-acl/tests/*` (nine files) | nothing yet | - | not ported, deliberately: see section 3 |

## 3. What is unrepresentable, and why

- **The Windows ACL backend.** Upstream's is about fifteen hundred lines across an FFI layer, a
  token builder, an ACL grant, a workspace SID, a path-boundary check and a runner. Nothing in this
  workspace can prove any of it: there is no Windows host in CI, so the only assertion available
  would be that it compiles - and a sandbox nobody has watched deny anything is not a sandbox.
  `crates/sandbox/src/unsupported.rs` refuses instead, naming the backend a port would restate, and
  TC-PORT-SANDBOX-11 pins that the refusal is a refusal and never an unconfined success.
- **Seatbelt on macOS.** The same argument, for the same reason.
- **Read confinement.** Landlock's allow-list governs effects; a policy that could hide files from a
  confined process needs a mount namespace or a container, which is a different capability seam
  (upstream says the same: containers and microVMs "replace the surrounding capability seam").
  `Policy::readable_roots` grants `/` and says why: a build that cannot read `/usr/lib` is not
  confined, it is broken.
- **The escalation flow.** Upstream's `escalation.spec.ts` is an approval protocol over a policy -
  a denied command retried once under a wider mode, with the user consenting through the approval
  prompt. Both halves exist here (`Policy`, and `tetanus_turn::approval` with its durable
  asked/decided pair) and wiring them is a slice of its own; inventing the retry semantics inside
  the sandbox crate would put a conversation policy in the wrong place.
- **Truncation before Landlock ABI 3.** A kernel below ABI 3 cannot govern truncation of a file
  whose write it denied, so a denied-write file can still be emptied. That is reported as
  `Enforcement::Partial` and refused unless the policy accepted partial enforcement, rather than
  rounded up to "sandboxed".

## 4. The named follow-up: the filesystem half

The brief for this lane assumed a landed `crates/fs`. This branch does not carry one, so nothing
here guesses at its API. What the follow-up needs is already in place:

- `Policy` is the shared vocabulary, and it is deliberately free of any process- or file-specific
  type, so a filesystem service takes the same value the executor takes.
- `landlock::confine_current_thread` is the in-process half of the backend, written and documented
  for exactly that caller: a service that confines *itself* rather than a child.
- The rule the two must share is upstream's, and it is why the derivation lives in `Policy`
  rather than in either consumer: if the file tools and the shell derive their writable roots
  separately, they will disagree, and the disagreement will read as a bug in whichever one the user
  noticed second.

Until that lands, a deployment that confines the shell still has unconfined file tools, and the
row in section 3 says so.
