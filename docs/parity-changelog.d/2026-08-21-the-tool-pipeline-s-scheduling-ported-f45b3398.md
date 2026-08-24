---
date: 2026-08-21
order: 61
---
The tool pipeline's scheduling ported as properties (`crates/turn/tests/properties_tools.rs`, TC-PROP-TOOL-1..6), the third `properties.spec.ts` this workspace has an answer for and the last item of the `core/*` gap that was a test gap rather than a missing surface. Until now the scheduler was pinned by five example schedules; a cap, a run of parallel calls and a barrier interact, and no example put a barrier in a pool that was still draining - which is exactly the `started > 0` break in `run_tool_group`, and exactly what the mutation check's shrunk counterexample is. Overlap is reconstructed from an ordered start/end trace rather than sampled with a peak counter, so a claim is decided at every instant the schedule reached. No engine change: it found nothing, which is the expected result for rules five examples already pin, and the value is that they now hold for schedules nobody wrote down.
