# Parity: the SPA dist server, and the index taps under it

Upstream packages: [`host/frontend-static`] (the fallback-seat owner) and the
`tapIndex`/`applyIndexTaps` half of [`host/webserver`].

tetanus: `crates/host`, `Frontend::mount` and `WebServer::tap_index`.

## What is here

| Upstream | tetanus | Same? |
| --- | --- | --- |
| claims the single fallback seat, second claim throws | `Frontend::mount` -> `Err(Taken::Fallback)` | yes |
| effect-scoped: disposing releases the seat, then 404 | dropping the guard does, asserted | yes |
| traversal outside the dist root is 403 | same, on the resolved path | yes, and wider |
| any miss falls back to `index.html` with 200 | same | yes |
| unknown extensions ship as `application/octet-stream` | same | yes |
| non-GET/HEAD without a named route is 405 | same, with an `allow` header | yes |
| every index response runs `applyIndexTaps` | same | yes |
| `tapIndex(transform)`, in order | `WebServer::tap_index`, in order, guard removes one | yes |
| `distIndex` is an assembly fact, never hardcoded | `mount` takes the index path from the caller | yes |
| the starter MIME table is minimal on purpose | 19 types, the bundler's set plus the manifest | yes |

## Deliberate differences

- **`..` is refused, not resolved.** Upstream checks the resolved path is
  inside the root. We do that too - a symlink out spells no `..` at all, which
  TC-HOST-STATIC-3 covers - but a written or escaped `..` is refused before
  resolution, because a `..` that lands inside the root today lands outside it
  the moment a directory moves.
- **A malformed `%` escape is 403, not 400.** The carrier answers 400 for a
  head it cannot parse; this is a head it parsed and a *path* it cannot read,
  and a path this server will not guess at is a path it will not serve.
- **Taps compose rather than exclude.** They are not a seat: several plugins
  each have something to tell the page. Upstream's are ordered by registration
  and so are these, with a guard that removes exactly the one it holds.

## Not here yet

- **The boot manifest itself.** The tap chain exists and is proven; what goes
  into it is the assembly's, and lands with the served frontend.
- **Conditional requests and caching.** No `etag`, no `last-modified`, no
  `cache-control`. A dev shell reloading a page it just rebuilt wants none of
  them, and a deployment that does wants a reverse proxy, which upstream's own
  limitations section says as well.
