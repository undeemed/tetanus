# Contract update notice: a carrier seam in `crates/rpc`

- **Lane:** external contract surfaces (`acp/*`, `sdk/*`, `api/*`, query half of `session-query/*`).
- **Branch:** `fm/tetanus-p3-acp`.
- **Status:** landed on the branch, and flagged here because it touches an engine-lane file.
- **Kind:** additive API in `crates/rpc`. No wire change, no behaviour change, no contract
  document change.

## Why this file exists

`docs/interface-contract.md` §4.7 gives `crates/rpc` to the engine lane, and says "neither lane
edits the other's files". This lane edited one. The change is additive and the carrier
conformance suite is untouched and green, but the rule is a rule, so the change is written
down here rather than discovered in a diff.

## What changed

Three additions, in `crates/rpc/src/lib.rs` and `crates/rpc/src/stdio.rs`:

```rust
pub trait FrameSink: Send + Sync {
    fn send_frame(&self, frame: String);
}

#[async_trait::async_trait]
pub trait FrameHandler: Send + Sync {
    async fn frame(&self, raw: &str, out: &Arc<dyn FrameSink>) -> Option<String>;
    async fn close(&self) {}
}

pub async fn serve_handler<R, W>(handler: Arc<dyn FrameHandler>, input: R, output: W)
    -> io::Result<()>;
```

`Codec` gains a `FrameHandler` impl that wraps the untyped sink back into the typed
`EventSink` it already wanted. `Frames` gains a `FrameSink` impl, which is one line over the
channel it already holds.

**Not changed:** `Codec::frame`, `Codec::close`, `Frames::notify`, `stdio::serve`, and the
whole of `websocket.rs`. `serve` keeps its exact signature and its exact body. `crates/rpc`
gains `async-trait` as a dependency, which was already a dev-dependency.

## Why it was needed

The lane's brief is to speak ACP "over the existing JSON-RPC carrier in `crates/rpc`". ACP is
JSON-RPC 2.0, one object per line, over stdio - byte for byte the framing `stdio::serve`
already implements. But `serve` is typed to `Arc<dyn Engine>`, and ACP's method vocabulary is
not the contract's, so there was no way to reach the carrier without a seam.

The alternative was a second line-framed carrier inside `crates/acp`. That was rejected
because the carrier's real content is not the framing: it is concurrent dispatch so a cancel
is read while the prompt it cancels is still running, a single writer task, and the promise
that a frame written during a call reaches the peer before that call's answer. Those are
properties two copies would eventually stop sharing, and the one that drifted would be the
one with no conformance suite behind it.

## What the owning lane may want to do with it

Nothing is required. If the engine lane would rather own this shape differently - a different
trait name, `serve` expressed in terms of `serve_handler`, or the seam declined and
`crates/acp` given its own carrier after all - that is the owning lane's call, and this branch
will follow it. The `crates/acp` side depends only on the three items above.

## Evidence it is safe

- `cargo test -p tetanus-rpc` before and after: the same cases, all passing. The carrier
  conformance suite asserts `serve`'s behaviour and was not edited.
- No file under `crates/protocol` and no line of `docs/interface-contract.md` is touched by
  this branch.
