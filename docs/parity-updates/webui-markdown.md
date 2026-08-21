# Parity: the markdown family

Upstream: `MarkdownText`, `MessageText`, `CodeBlock` and `JsonBlock` in
[`client/ui-primitives`].

tetanus: `web/app/markdown.js`, with `messageText` and `jsonBlock` already in
`primitives.js`.

## The security posture is the part that was ported

Upstream's renderer is GFM plus KaTeX plus shiki. What matters more than any of
that is what it refuses to do with untrusted text, and every one of those
refusals is here:

| Upstream | tetanus | How |
| --- | --- | --- |
| omits raw HTML | same | there is no `innerHTML` anywhere: the renderer builds elements, so there is no parser to confuse and no sanitiser to bypass |
| neutralizes relative and non-HTTP(S)/mailto links | same | `http`, `https`, `mailto` become anchors; everything else keeps its text and gets no anchor |
| opens HTTP(S) links with safe external-link attributes | same | `target="_blank"`, `rel="noopener noreferrer nofollow"` |
| renders absolute HTTP(S) images without a referrer | **narrower**: no images at all | an image tag is a request to a server of the model's choosing, and this build has no reason to make one. The alt text is kept, so only the fetch is lost |
| `MessageText` stays the literal-text primitive | same | a person who types `*hello*` meant the asterisks |
| fenced blocks render through `CodeBlock` with a language banner and a copy control | same, without shiki | no highlighting; the banner and the copy are the parts a reader uses |

The scheme is **parsed, not prefix-matched**. `JaVaScRiPt:alert(1)` is the same
trick as `javascript:` and a page comparing strings catches neither; the URL
parser catches both, and a case asserts it.

A relative path is refused by shape rather than by scheme, because a relative
path resolves against the page's own origin and comes out `https:` - this page
has no pages of its own for a model to link to.

## What it renders, and what is left

Fenced code, headings, ordered and unordered lists, block quotes, horizontal
rules, paragraphs; inline code, bold, italic, links and autolinks.

Not here, each its own slice and none of them changing the posture above:
tables, footnotes, task lists, KaTeX math, and upstream's incremental
tail-parsing cache - which exists for streaming, and this page streams as
plain text until the message settles, so a half-written fence is never rendered
as a document.

## Tests

`target/probe-primitives.mjs`, **76/76**. The ones worth naming: raw HTML from a
model stays text and builds no element; `javascript:`, `data:` and a relative
path keep their text and lose the link while `http` and `mailto` keep it; every
anchor carries the safe attributes; a mixed-case scheme is still refused; an
image is not fetched and its alt text survives; and an answer that stopped
mid-fence still shows the code it did write.

Verified in Chrome: one anchor, one inert link, one code block with its
language and a copy control, one heading. Screenshot at
`target/shots/webui-markdown.png` and in the handoff dir, **50641 bytes** in
both.
