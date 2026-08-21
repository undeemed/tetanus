# Parity: the chooser the browse backend exists for

Upstream: [`host/directory-picker-browse`]'s browser half - the in-app **Select
Workspace Directory** dialog that fills ui-workspace's two directory-flow
holes, driving `host.listDirectory` and `host.createDirectory`.

tetanus: the `workspace…` dialog in `web/app`.

## What is here

| Upstream | tetanus | Same? |
| --- | --- | --- |
| an in-app dialog, for clients that cannot reach an OS chooser | same | yes |
| a Miller two-column view: the level and its parent | same, two panes | yes |
| the target re-selected as its parent-level entry, so stepping back never collapses | same, the level we came from is marked in the parent pane | yes |
| a breadcrumb of jump targets | same, every crumb the host named | yes |
| a show-hidden footer toggle over the host's `hidden` flags | same | yes |
| a nested New-folder dialog | a prompt, and the host's own refusal shown | narrower |
| driven by `host.listDirectory` / `host.createDirectory` | same, and nothing else | yes |
| the display root lists alone | same: no parent pane, one wide level | yes |

## The rule this follows

Everything the host said is drawn, and nothing is inferred. `hidden` is a flag
on the row rather than a name this page re-derives, so the toggle acts on the
host's answer and a platform that ever means something else by hidden changes
in one place. `truncated` is said out loud - `the level is longer than this` -
rather than quietly shown as a short level, because a chooser that hides its
own incompleteness sends a reader looking for a directory that was there all
along. The picker's three failures are printed as the host worded them, since
each already says what to do next.

The parent leg is best-effort: a level whose parent cannot be read is still a
level worth showing, and the pane is simply not there.

## Not here yet

Upstream's dialog carries a great deal more, and each piece is a slice rather
than an omission: the click-to-edit path zone whose draft filters the last pane
and navigates on a rest, the 300ms silence window before a loading pill, the
200ms parent-leg wait that lands the target alone and upgrades in place, the
dot-led typed prefix filter, and the selection-anchored quiet navigation that
keeps the previous view rendering while a scan runs. This slice is the two
panes, the crumbs, the toggle and creation - the parts a reader needs to choose
a directory at all.

## Verified

In real Chrome, loaded from `http://15.204.113.4:5400/`: the dialog opens at
the account's home with 57 directories, `Dev` descends to a four-directory
level with `Dev` marked in the parent pane, the crumbs read `/ home ubuntu Dev`
and jump, and the toggle reveals `.omp` as a hidden row. Screenshot at
`data/tetanus-ui-handoff/web-5400-picker.png`.
