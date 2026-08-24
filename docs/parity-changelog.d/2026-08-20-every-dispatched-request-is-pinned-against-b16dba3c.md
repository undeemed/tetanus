---
date: 2026-08-20
order: 42
---
Every dispatched request is pinned against the journal it went out on (TC-PORT-REQ-1..3). The suites so far asserted one request at a time; nothing said that the sequence only ever grows, or that `derive_messages` over the log prefix reproduces what the adapter was handed. Both are what makes a replayed session honest rather than merely re-readable.
