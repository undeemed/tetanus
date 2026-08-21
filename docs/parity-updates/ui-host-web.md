# Parity: the panel promoted into the served frontend

Upstream: the composed web shell - [`apps/web`] served by [`host/frontend-static`]
on [`host/webserver`], with the boot manifest arriving through `applyIndexTaps`.

tetanus: `tetanus web` (`crates/cli/src/web.rs`), serving `web/app` through
`crates/host`.

## What changed

| Before | Now |
| --- | --- |
| `web/chat/serve.py`, a Python dev server | `tetanus web`, the product binary |
| the carrier's address string-replaced into the HTML as it was served | a boot manifest written by an index tap, read by the page as data |
| files served by `http.server` | the host's fallback seat, with the shell's locked semantics |
| `window.TETANUS_WS`, patched in | `window.TETANUS_BOOT`, a manifest with `carrier` and `protocol` |
| a deep link was a 404 from `http.server` | a miss is the page with 200, so the router in the browser owns it |

The page kept `?ws=` as an override, because opening it against a server
somebody else started is a real thing to do, and it still reads
`window.TETANUS_WS` so an older embedding does not break.

## Why the manifest and not the patch

A page patched on its way past only works when served by the one program that
patches it. The manifest is a published seam - upstream's `tapIndex` - so the
page runs from any assembly that writes one, and the page reads it as data
rather than being rewritten.

`<` is escaped inside the manifest, so a carrier address containing
`</script>` arrives as data rather than closing the tag and turning the rest of
the manifest into markup. TC-HOST-STATIC-7 asks for exactly that.

## Deliberate differences

- **Two ports, for now.** Upstream serves the page and the socket from one
  server, the socket on an upgrade route. Ours binds the carrier separately and
  names it in the manifest. Folding it onto the one port needs the raw socket
  handed to the upgrade handler, which is the next slice; the page is told an
  address either way, which is why it was told one in the first place.
- **`--frontend` is a flag with a default.** Upstream resolves `distIndex`
  through the frontend package's exports and says a deployment never hardcodes
  it. A flag is the same fact in a binary with no package graph, and the
  default is the directory in this repository.
- **`TETANUS_PUBLIC_HOST`** names the address a reader off the machine reaches
  when the bind is `0.0.0.0`, which is every interface and nobody's hostname.
  Upstream leaves the URL line to its shell for the same reason.

## Tests

| Id | Case | Expected result |
| --- | --- | --- |
| TC-HOST-STATIC-7 | a manifest whose value would close the script tag | inside the head, before the page's script, `<` escaped |
| TC-CLI-WEB-1 | `tetanus web --frontend nowhere` | exit 1, the directory named, nothing bound |
| TC-CLI-WEB-2 | `--listen 192.168.1.10:5300` | refused, naming the two addresses it binds |
