# Coverage baseline - tetanus workspace

**Identification.** Measured coverage of the tetanus workspace at commit `e57962f`
(`master`, 2026-09-01), produced by `cargo-llvm-cov` 0.9.0 on
`rustc 1.97.1`.
Authoritative copy: `data/tetanus-coverage-100/baseline.md` on branch
`fm/tetanus-coverage-100`.
This document is the measured starting point for the drive to full coverage.
It states what is measured, what is *mis*-measured, and what is not measured at
all, because two of the three change what the next lane should do.

**Stakeholders and concerns.**

| Reader | Question this answers |
| --- | --- |
| The captain | What is the real number today, and is it trustworthy? |
| A lane picking up work | Which crate is mine, what does it cost, why is it in that order? |
| A reviewer | Does a given percentage mean the code is guarded, or only executed? |

## 1. How it was measured

```
export CARGO_TARGET_DIR=/home/ubuntu/.cache/tetanus-target/cov
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_TEST_DEBUG=0     # coverage lives in instrprof, not DWARF
cargo llvm-cov --no-report --workspace test --no-fail-fast
cargo llvm-cov report --show-missing-lines
```

`--no-fail-fast` matters: without it the run stops at the first failing test
binary and the report covers only the crates that ran before it.

`CARGO_PROFILE_TEST_DEBUG=0` is safe here and roughly halves the artifact size.
llvm-cov reads the `__llvm_covmap`/`__llvm_prf_*` sections, not DWARF, so
dropping debug info costs line numbers in a backtrace and costs the measurement
nothing. The same trade CI already makes, for the same disk reason.

**Run result.** 224 test binaries, 2,237 cases passed, 1 failed.
The one failure is `tetanus-exec --test upstream_screen`
`htop_is_readable_and_its_transcript_is_not`, a member of the known
load-flaky terminal family `AGENTS.md` already names. It is not caused by
anything in this branch and it passes when run alone.

## 2. The headline number

| Metric | Total | Covered | Missed | Percent |
| --- | ---: | ---: | ---: | ---: |
| **Lines** | 37,121 | 32,815 | **4,306** | **88.40%** |
| Regions | 57,167 | 49,966 | 7,201 | 87.40% |
| Functions | 4,917 | 4,233 | 684 | 86.09% |

This is Rust only. It excludes 4,821 lines of browser JavaScript entirely - see
section 5.

**Read this number down, not up.** Section 4 shows it is understated in the
server paths and overstated as a guarantee everywhere, for two different
reasons.

## 3. Per crate, worst first

| Crate | Lines | Missed | Line % | Region % | Integration files | Src files with inline unit tests |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sdk | 543 | 131 | 75.9% | 71.0% | 2 | 0 |
| mcp | 1,276 | 288 | 77.4% | 76.8% | 5 | 0 |
| **sandbox** | 325 | 71 | **78.2%** | 82.6% | 1 | 1 |
| toolset | 491 | 98 | 80.0% | 77.8% | 2 | 0 |
| coderuntime | 1,871 | 361 | 80.7% | 76.6% | 5 | 0 |
| fs | 1,548 | 235 | 84.8% | 84.3% | 7 | 0 |
| host | 585 | 84 | 85.6% | 83.3% | 1 | 1 |
| cli | 8,011 | 1,145 | 85.7% | 85.9% | 12 | 19 |
| exec | 4,216 | 585 | 86.1% | 85.7% | 14 | 0 |
| hooks | 1,210 | 159 | 86.9% | 86.0% | 13 | 0 |
| acp | 909 | 118 | 87.0% | 81.5% | 2 | 0 |
| core | 1,896 | 212 | 88.8% | 87.3% | 11 | 0 |
| rpc | 498 | 54 | 89.2% | 87.1% | 4 | 0 |
| config | 940 | 92 | 90.2% | 90.0% | 12 | 0 |
| web | 930 | 75 | 91.9% | 91.3% | 3 | 0 |
| query | 651 | 49 | 92.5% | 93.1% | 2 | 0 |
| engine | 1,863 | 118 | 93.7% | 92.0% | 25 | 0 |
| features | 1,147 | 69 | 94.0% | 92.6% | 6 | 0 |
| ui | 1,860 | 111 | 94.0% | 94.3% | 6 | 6 |
| session | 823 | 43 | 94.8% | 91.4% | 5 | 0 |
| turn | 4,994 | 194 | 96.1% | 94.8% | 53 | 1 |
| subagent | 457 | 14 | 96.9% | 96.2% | 7 | 0 |
| protocol | 77 | 0 | 100.0% | 97.1% | 2 | 0 |
| **TOTAL** | **37,121** | **4,306** | **88.40%** | **87.40%** | 204 | 29 |

**Integration versus unit, as asked.** Every crate has at least one integration
test file, so no crate is unit-only. The reverse is the real finding: 20 of 23
crates have **no** inline `#[cfg(test)]` unit tests at all and are guarded
purely from outside the crate boundary. Only `cli` (19 files), `ui` (6),
`host`, `sandbox` and `turn` (1 each) test anything from the inside. That is
why the uncovered remainder is concentrated in private helpers and error
branches that an integration test cannot reach without contorting the public
surface - and it is the structural reason the last 12% is harder than the
first 88%.

### Worst files by missed lines

| Missed | Line % | File | Genuine gap? |
| ---: | ---: | --- | --- |
| 309 | 69.6% | `cli/src/main.rs` | partly - see §4.1 |
| 264 | **0.0%** | `cli/src/bridge.rs` | **no - artifact, §4.1** |
| 255 | 48.1% | `cli/src/chat.rs` | partly |
| 225 | 64.6% | `coderuntime/src/local/eval.rs` | yes |
| 145 | **0.0%** | `mcp/src/bin/fixture.rs` | **no - artifact, §4.1** |
| 122 | 45.0% | `cli/src/web.rs` | **mostly artifact, §4.1** |
| 114 | 70.1% | `hooks/src/bridge.rs` | yes |
| 104 | 69.9% | `exec/src/screen.rs` | yes |
| 89 | 80.4% | `fs/src/local.rs` | yes |
| 88 | 75.9% | `acp/src/client.rs` | yes |
| 77 | 84.8% | `exec/src/terminal.rs` | yes |
| 75 | 72.7% | `ui/src/terminal.rs` | yes |
| 73 | 81.3% | `exec/src/pty.rs` | yes |
| 63 | 66.5% | `sandbox/src/landlock.rs` | yes - **highest risk** |
| 60 | 88.8% | `exec/src/session.rs` | yes |
| 60 | 69.2% | `sdk/src/gateway.rs` | yes |
| 59 | 84.4% | `coderuntime/src/local/program.rs` | yes |
| 51 | 90.6% | `exec/src/tools.rs` | yes |
| 50 | 88.4% | `exec/src/proc.rs` | yes |
| 49 | 86.4% | `core/src/schedule.rs` | yes |
| 49 | 87.3% | `exec/src/terminal_tools.rs` | yes |
| 46 | 84.5% | `host/src/lib.rs` | yes |
| 45 | 78.7% | `sdk/src/client.rs` | yes |
| 41 | 78.4% | `toolset/src/lib.rs` | yes |
| 40 | 94.0% | `cli/src/render/pick.rs` | yes |

## 4. Blind spots - read before trusting any number above

### 4.1 A SIGKILLed server records no coverage. The number is understated.

`cli/src/bridge.rs` reports **0.0% over 264 lines**. It is not untested.

`crates/cli/tests/presentation.rs` TC-CLI-WEB-5
(`the_api_bridge_answers_the_published_contract_over_http`) **passes**. It POSTs
`/api/<method>` over a real TCP socket to a real `tetanus serve --frontend`
child and asserts 415-before-dispatch, the handshake, a call after it, and an
unknown method. A server that had not run `bridge::mount` could not answer any
of that.

The proof that this is measurement and not testing: `cli/src/web.rs:194` is the
call `crate::bridge::mount(&page, ...)`, and the report marks line 194
**uncovered** while the test that depends on its having run passes.

The mechanism: the LLVM profile is written by an `atexit` handler. Those tests
stop the server with `std::process::Child::kill()`, which is `SIGKILL`, so the
child dies without writing `.profraw` and everything it executed is lost.
`cli/src/main.rs` still shows 69.6% because the short-lived subcommands
(`tetanus tools`, `tetanus models`) exit cleanly and do write theirs.

Six test files kill a child this way, 13 call sites:
`cli/tests/{presentation,catalogue_agrees,mcp_boot,serve}.rs`,
`exec/tests/upstream_piped.rs`, `ui/tests/killed.rs`.
Everything reached only through those children is invisible:
`cli/src/bridge.rs` (264), `cli/src/render/web.rs` (18) and most of
`cli/src/web.rs` (122). With `mcp/src/bin/fixture.rs` (145), lost to the
separate cause in §4.1b, that is **at least 549 lines** currently counted as
missed which are in fact exercised.

**Consequence for the campaign, and it is the expensive one:** a lane told to
"fix `bridge.rs`, it is at 0%" would write a second set of tests for code
TC-CLI-WEB-5 already guards, and the number would improve for no gain in
safety. Do not assign these files as coverage work.

**The fix is to the harness, not to the tests.** Either give
`tetanus serve` a shutdown path the tests use instead of `kill()`, or enable
LLVM continuous mode (`-C instrument-coverage` with `%c` in `LLVM_PROFILE_FILE`
plus runtime counter relocation), which mmaps the counters and survives
`SIGKILL`. Until one of those lands, treat any `serve`-only path's percentage
as unknown rather than as zero. This is its own slice - it is listed as
group 0 in the plan and it should land before the crates it distorts.

### 4.1b A child with a cleared environment loses its profile too

A second, independent reason the number is understated, found the same way -
by an artifact that should not have existed.

`crates/exec/src/piped.rs:145` calls `.env_clear()` on every child spawned
through the piped seam, which is every MCP server. That is correct for the
seam's own purpose and it also strips `LLVM_PROFILE_FILE`, the variable
cargo-llvm-cov uses to tell a child where to write its counters. With the
variable gone the LLVM runtime falls back to its default, `default_*.profraw`
in the **current working directory**.

Two consequences:

- `mcp/src/bin/fixture.rs` reports **0.0% over 145 lines** although
  `crates/mcp`'s process suite runs it constantly. Like `bridge.rs`, it is
  measured wrong, not untested. Do not assign it as coverage work.
- The instrumented run **writes into the source tree**. This baseline run left
  13 `default_*.profraw` files in `crates/mcp/`, and `.gitignore` did not cover
  them, so a `git add -A` during a coverage run commits them. That happened
  once while producing this document and was caught before the commit.
  `.gitignore` now ignores `*.profraw`; the underlying measurement gap is still
  open and belongs to group 0.

The fix is the same family: pass `LLVM_PROFILE_FILE` through the piped seam
when it is set, or use continuous mode. Note that this one is *not* fixed by
giving `serve` a graceful shutdown - the two artifacts have different causes
and each needs its own answer.

### 4.2 Browser JavaScript is not measured at all. 0%, honestly.

| | |
| --- | ---: |
| JavaScript modules in `web/app` | 18 |
| JavaScript lines | **4,821** |
| Lines executed by any test | **0** |
| JS test runner in the repo | **none** |

There is no `package.json`, no `vitest`/`jest` config, and no `*.test.js`
anywhere in the tree. Not one line of browser JavaScript is ever executed by
the suite. The full `web/app` directory is 5,794 lines including HTML, CSS and
its README; the executable JavaScript is 4,821 of those.

What guards it today is 13 Rust cases in `crates/host/tests/web_app.rs`
(TC-WEB-1..15). Twelve of them are **text scans** over `include_str!` of the
sources - they check that a string is present, not that the code does anything.
The file says so about itself.

The one case that does more is **TC-WEB-12**, which shells out to `node --check`
on each module renamed `.mjs` to prove it at least *parses*. Two things about
it, both material:

- It **skips when `node` is absent**, falling back to the much narrower
  duplicate-declaration scan.
- **CI has no `node`.** `.github/workflows/ci.yml` installs a Rust toolchain and
  nothing else. So on every pull request this workspace has ever merged,
  TC-WEB-12 has taken the fallback path and **the parse guard has never run in
  CI**. It runs on this box only because a `node` happens to be on `PATH`.

So the honest statement is: **JavaScript coverage is 0%, and even the syntax
gate is off in CI.** A module that fails to parse ships a blank panel with an
empty console, which is the exact failure TC-WEB-12's own docstring records as
having happened before.

Folding this into the Rust figure would give a combined
32,815 / 41,942 = **78.2%** for the executable code in the repository. The
88.40% above is the Rust-only number and should always be quoted as such.

### 4.3 Percentage is not protection

Coverage says a line ran. It does not say anything asserted its effect. A
separate, countable signal: **56 error enum variants across the workspace are
never named by any test** - not in an assertion, not by their `code::` constant,
not in a match. Those are error branches that may be *constructed* but whose
behaviour nothing pins.

| Crate | Variants | Never named |
| --- | ---: | ---: |
| turn | 71 | 19 |
| exec | 36 | 9 |
| features | 28 | 9 |
| core | 39 | 3 |
| fs | 18 | 3 |
| host | 3 | 3 |
| query | 4 | 2 |
| sandbox | 4 | 2 |
| acp / coderuntime / config / hooks / session / web | 4/6/15/3/8/13 | 1 each |
| engine, mcp, sdk, subagent, toolset | 32 | 0 |
| **TOTAL** | **271** | **56** |

A note on method, because a wrong proxy here would misdirect the whole
campaign: an earlier pass of this count matched only `Enum::Variant` spellings
and reported `web` as 11 of 13 untested. That was **wrong** - `crates/web`
asserts through `fault::code::TOO_LARGE` constants, so the variants are pinned.
The table above also counts the `SCREAMING_SNAKE` form and any `::Variant`
match, and `web` is 1, not 11.

## 5. Staged plan - ordered by risk, not by ease

Risk here means the cost of a *silent* defect: code where a bug produces a
wrong answer nobody notices, rather than a crash somebody reports. A sandbox
that quietly grants a write outranks a renderer that draws a wrong glyph, even
though the renderer has more uncovered lines.

Lanes are grouped so that no two touch the same files. Cost is a rough order of
magnitude in lane-days, not a promise.

| # | Group | Crates / paths | Missed lines | Cost | Why here |
| --- | --- | --- | ---: | --- | --- |
| **0** | **Fix the measurement** | harness only: `serve` shutdown, `LLVM_PROFILE_FILE` through the piped seam, or LLVM continuous mode | unblocks ~694 | 0.5 | Everything after this is measured against a broken instrument. Two distinct causes, §4.1 and §4.1b. Must land first or three lanes will chase artifacts. |
| **1** | **Sandbox** *(this PR)* | `crates/sandbox` | 71 | 1 | The only thing between a model-written command and the filesystem. A silent hole here is unbounded and undetectable from inside. Its ABI-degradation logic is currently untestable on one kernel - restructure, do not waive. |
| **2** | Process & signal containment | `crates/exec` (`pty`, `proc`, `session`, `screen`, `terminal*`) | 585 | 3 | Escaped process groups, undeliverable interrupts, a credential on a journal. 9 error variants unnamed. Already the source of the fleet's flaky-test tax. |
| **3** | Path confinement & durability | `crates/fs`, `crates/core` (`jobs`, `schedule`, `registry`, `spill`) | 447 | 2.5 | A path escape and a corrupted append-only journal are both silent and both unrecoverable. `RegistryError::Cycle` is unnamed - a cycle bug is a hang. |
| **4** | Contract surfaces | `crates/sdk`, `crates/mcp`, `crates/toolset`, `crates/acp` | 635 | 2.5 | Lowest coverage in the workspace (75.9%, 77.4%, 80.0%, 87.0%). These are the published seams; a wrong answer here is wrong for every client at once. |
| **5** | Evaluator budgets | `crates/coderuntime` | 361 | 2 | Fuel and wall clock are the only reason a runaway program stops. Two cases here race their own budgets - fixed in this PR. |
| **6** | Config, secrets, hooks | `crates/config`, `crates/hooks`, `crates/session` | 294 | 2 | `CredentialError::Unwritable` unnamed; `hooks/src/bridge.rs` at 70.1%. A hook that silently does not fire is a policy that silently does not apply. |
| **7** | Engine & turn remainder | `crates/turn`, `crates/engine`, `crates/query`, `crates/features` | 430 | 3 | Already 93-96%. High absolute value, low marginal risk, and 19 unnamed `turn` variants are the real target rather than the percentage. |
| **8** | Presentation | `crates/cli`, `crates/ui`, `crates/host`, `crates/rpc`, `crates/web` | 1,469 | 4 | Largest remainder, smallest blast radius. **Do not start before group 0** - roughly a third of `cli`'s apparent gap is the artifact. |
| **9** | **JavaScript, from zero** | `web/app` (18 modules, 4,821 lines) | 4,821 | 5+ | Own decision before own work: it needs a runtime and a runner this project has so far declined to take on. At minimum, put `node` in CI so TC-WEB-12 stops silently skipping. §4.2 |

**Ordering constraints.** Group 0 gates groups 8 and 4. Nothing else is
ordered against anything else, so groups 1-7 can run concurrently in any mix.
Group 9 needs a decision from the captain before a lane is spent on it.

### What a group is done means

Not a percentage. For each unit in the group:

- every error branch **taken**, not merely constructed;
- boundaries from both sides - empty, one, many, max, max+1, zero, negative,
  overflow;
- malformed, hostile and truncated input **refused with the named error**
  rather than panicking;
- ordering and concurrency asserted wherever the code claims either;
- the seam exercised for real - a real engine, a real carrier, a real temp
  directory - not a mock that agrees with the implementation;
- and every new case proven to **bite**, by mutating the code it names and
  confirming it fails.

A line that genuinely cannot be reached is deleted as dead or the code is
restructured until it can be. It is never waived silently.

## 6. Reproducing this

The JSON the tables were generated from is not committed - it is 25 MB. Rebuild
it with the commands in section 1 plus:

```
cargo llvm-cov report --json --output-path cov.json
```

Note for whoever runs it next: the workspace shares one box. Point
`CARGO_TARGET_DIR` at a directory of your own, and before reclaiming anybody
else's, run `pgrep -af <that path>` first - a process still executing out of a
deleted build directory keeps working off the freed inode and fails later,
somewhere else.
