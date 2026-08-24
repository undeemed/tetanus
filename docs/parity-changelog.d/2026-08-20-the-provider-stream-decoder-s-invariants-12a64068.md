---
date: 2026-08-20
order: 48
---
The provider stream decoder's invariants ported as properties (`crates/turn/tests/properties_stream.rs`, TC-PROP-STREAM-1..6). Model based: a case generates the frames as data, renders them onto the wire and folds them a second time itself, so the assertion is against a restatement of the rule rather than against the decoder's own output. No adapter change: the recorded streams in `deepseek_adapter.rs` had already found the two defects here (a cut stream, and a frame after the sentinel), and the properties now state them for every stream.
