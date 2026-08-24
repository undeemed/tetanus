---
date: 2026-08-19
order: 27
---
A journal root that cannot be read is answered as one: `session.list` carries the path in its `Io` failure (contract §4.5), and only the default root reads as an empty history (TC-SESS-7..9). Before this, a root that was a file lost the path and a mistyped root reported no sessions yet. Reported by the presentation lane as issue #150.
