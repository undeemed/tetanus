# Note: the served catalogue is one answer (slice `catalogue-agreement`)

Not a parity port. A defect found by the presentation lane on a landed build,
and the shape it had.

## 1. What was wrong

On a twenty-six-tool build, `catalog.tools` answered with one tool - `echo` -
over `tetanus serve --frontend`, and with twenty-six over `tetanus serve`.
Every client behind the frontend saw a near-empty toolbox: the browser panel,
and anything posting to `/api/`.

## 2. Why one assembly did not make one answer

`crates/toolset` is the single declared set of tools this build offers, and
that part was working. What is *served* comes from `EngineConfig::tools`, and
the binary builds an engine in five places: the two carriers of `tetanus
serve`, the frontend's carrier, the sessions page, and the config page. Only
the first was given the assembly. The composition was written out at that call
site as `EngineConfig { tools, session_tools, ..booted }`, so `web::web` - a
second serving surface, added by another lane - did the obviously reasonable
thing with the `booted` it had and got `EngineConfig::default()`.

Two comments in `crates/cli` and one in `crates/toolset` stated the listing and
the dispatch registry could not disagree. They could. The lesson is narrower
than "share code": **a comment saying two things cannot disagree is worth
nothing when agreeing is a property of one call site.** It has to be a property
of a function both callers go through, or of a case that compares them.

## 3. The fix, and the case

`crates/cli/src/tools.rs::served` is now the one answer to "what are a served
engine's tools", and both serving surfaces go through it. The two toolless
surfaces - `session.list`, `config.dump` - deliberately do not, and say so
where they are built, because neither reports a tool and neither should pay to
compose one.

TC-CLI-CAT-12 (`crates/cli/tests/catalogue_agrees.rs`) asks one build for its
toolbox five ways - the tools page, `tetanus info`'s count, stdio, the
WebSocket carrier, the frontend's carrier - and compares the answers **to each
other**. One case and not five, because five that each pass in isolation is
exactly how this survived: `tetanus serve` had a catalogue case and was right,
`tetanus serve --frontend` had none and was wrong, and a case per surface
cannot catch the surface nobody wrote a case for.

It asserts agreement rather than a count, so a tool crate landing does not have
to edit it. The residual risk is named in `AGENTS.md`: a *new* surface still
has to be added to the case, and adding it is part of adding the surface.

## 4. Rows to fold

### Section 3, the surface vocabulary row

No change: what the surfaces serve is unchanged. This was a defect in one of
them, not a capability.

### Changelog row

Appended to `parity-changelog.md` by this branch, since that file is
`merge=union` and append-only.
