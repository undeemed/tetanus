---
date: 2026-08-20
order: 45
---
A record a crash cut short is dropped rather than refused with the journal that holds it (`crates/session/src/lib.rs`, TC-PORT-SESS-8..10, opening `jsonl.spec.ts` in section 4). Until now any unparsable line failed the whole read, so a session interrupted mid-append could not be opened again - and the repair in `crates/engine` that closes an interrupted turn never got the chance to run. The newline is the commit: an append writes one record and fsyncs it, so a file that does not end in one ends in a fact no caller was told was durable. Reopening truncates those bytes, so the next append lands on a record boundary and leaves no seq gap. A damaged line the writer did finish is still refused, by line number.
