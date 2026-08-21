# Parity: the directory-picker seam, its browse backend, and its two methods

Upstream: [`host/directory-picker`] (the capability seam),
[`host/directory-picker-browse`] (the in-app backend), and the gateway that maps
the backend's typed errors "1:1 onto wire error codes".

tetanus: `crates/host/src/picker.rs`, reached over the bridge as
`host.listDirectory` and `host.createDirectory`.

## What is here

| Upstream | tetanus | Same? |
| --- | --- | --- |
| `capability()` answers a discriminated union | `Capability`, one variant, an enum on purpose | yes |
| consumers hide picking for a kind they do not know | an enum keeps a consumer compiling and choosing | yes |
| `browse`: `list(path?)`, `createDirectory(path, name)` | `Browse::list`, `Browse::create` | yes |
| listings are directories only, name-sorted | same | yes |
| symlinks to directories followed, broken ones skipped | same - the probe failing is what "not enterable" means | yes |
| host-owned `hidden` flag, POSIX dot convention, client acts on it | same: reported, never applied | yes |
| `crumbs` is the root-to-target chain, root labelled by its full path | same | yes |
| absent `list` path means the host account's home | same | yes |
| creation is non-recursive; a missing parent is a real failure | same | yes |
| the name is one non-blank segment, checked in the backend too | same | yes |
| both primitives refuse a path that is not fully qualified | same | yes |
| at most `maxEntries` rows, default 1000, `truncated: true` when cut | same, and the bound is the default | yes |
| typed errors mapped 1:1 onto wire codes | `Io` / `InvalidParams` / `Io`, subject path in `data` | in this contract's spelling |

## Deliberate differences

- **The error codes are §4.5's, not a new set.** Upstream has
  `directory-unreadable` / `directory-exists` / `directory-create-failed`; this
  contract has no per-domain code space, so an unreadable path is `Io` with the
  path in `data` - which is the field §4.5 gives `Io` for exactly this - and
  "already there" is `InvalidParams`, because the machine is fine and the
  argument was wrong. Adding three codes would be a contract change, and that
  is a different lane's PR.
- **The bound is applied while reading, not after.** A level is sorted and cut
  as it goes, so a directory with a million children costs the bound rather
  than the directory. Upstream describes a bounded window with the same
  intent.
- **The probe is last.** `is_dir` runs only on rows that survived the cut, so
  the expensive syscall is paid for the rows a reader will actually see.
- **`host.*` is routed by the bridge, not the codec.** Upstream splits
  `HostApi` from `SessionsApi` for the same reason: choosing a directory is a
  question about this machine, not about a conversation, and the engine's
  method table should hold nothing the engine cannot answer.

## Not here yet

- **The native backend and `-auto`.** A remote browser cannot reach an OS
  chooser, so `browse` is the backend a served host needs; `native` waits for a
  shell that has a display to open one on.
- **The client-side dialog.** Upstream's Miller two-column browser with the
  editable breadcrumb is a frontend of its own; these are the primitives it
  would be built on.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-HOST-PICK-1 | a tree with a file, a dot directory, a good link and a dead one | directories only, name-sorted, dead link dropped, hidden reported |
| TC-HOST-PICK-2 | a nested path | crumbs root-first, every one absolute, the root labelled |
| TC-HOST-PICK-3 | forty children, a bound of ten | the sorted head of ten, `truncated` |
| TC-HOST-PICK-4 | create: ordinary, existing, missing parent, four bad names | made; `Exists`; `CreateFailed`; `CreateFailed` |
| TC-HOST-PICK-5 | a relative path, listing and creating | refused before the filesystem is touched |
| TC-HOST-PICK-6 | `capability()` | `Browse` |
| TC-CLI-WEB-6 | the two methods over the bridge, and two failures | 200 throughout, codes with the subject path in `data` |
