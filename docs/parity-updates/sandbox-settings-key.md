# Parity update: the settings key that confines a deployment

Written by the process-execution lane, for the reconciliation slice to fold into
[../parity.md](../parity.md). Nothing here edits that file: every lane collides there.

The `sandbox/*` row's Gap has named **a settings key** since the sandbox slice landed. This serves
it, and with it the one thing that made "one policy, both seams" a statement about `crates/exec`
rather than about this harness.

## 1. Section 3, the `sandbox/*` row

**Today** gains:

> The policy is a document's answer, settled by the engine with every other key: `sandbox.mode`,
> `sandbox.workspace` and `sandbox.network`, reported by `tetanus config` with the layer that
> decided each, and refused - with nothing run - when a mode is misspelled. The composition hands
> that one value to every child it starts: one-shot commands, persistent shells, terminals and
> configured hooks alike.

**Gap** loses *a settings key*, and keeps: the Windows ACL backend, parallel file operations behind
the boundary, and read confinement needing a container.

## 2. Why three keys

A mode without a root is half an answer - `workspace-write` has to write *somewhere*, and a harness
started from one directory while the work happens in another would refuse writes with nothing for
the operator to change. The network decision is separate because the policy already carries one for
its own reason: Landlock governs TCP from ABI 4, so a deployment that wants an offline build has
nowhere else to say so.

## 3. Why a misspelling is fatal

`sandbox.mode: read_only` is refused and the run does not start (TC-CLI-SANDBOX-4). Every other
unparsable value in this workspace is refused too, but this one is worth stating on its own: the
alternative failure - ignore the value, run unconfined - looks exactly like a correct configuration
from the outside, for as long as nobody audits it. A confinement that silently is not one is worse
than no confinement, because somebody is relying on it.

## 4. Section 4 rows

Add:

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `sandbox/sandbox-policy`'s settings resolution | `crates/cli/tests/sandbox_settings.rs` | A document's mode reaching the children this binary starts | ported: TC-CLI-SANDBOX-1..4. Upstream resolves its mode per session event (`effectiveSandboxMode`), which needs the session-scoped mode changes its terminal fence guards; here it is settled once at boot, and changing it mid-session is not served - named below |
| — (a hole this lane left itself) | `crates/exec/tests/upstream_sandbox_exec.rs` | A terminal under the policy, and its children | TC-PORT-SANDBOX-32. The pty layer grew a confined spawn when terminals landed and nothing asserted it *through* a terminal; every other sandbox case ran a command or a pipe-backed session |

## 5. What this does not serve

- **Changing the mode inside a session.** Upstream folds `sandbox/mode` events out of the session
  and re-resolves per call, which is why its terminal backend carries a fence refusing a mode change
  while a PTY is open. Here the policy is settled at boot and a session runs under one value. The
  fence has nothing to guard yet, and building the event without the fence would be the unsafe half
  of the feature: a terminal opened under one mode would go on running while the document said
  another.
- **A per-preset policy.** An agent preset can name tools and a persona but not a confinement. It is
  a plausible next step - the preset roster and the policy are both settled at boot - and it is not
  invented here, because "which agent may do what" is the agent registry's question and that row is
  still being built.
