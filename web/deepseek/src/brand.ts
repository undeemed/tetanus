// The product surface, and only the product surface.
//
// Upstream's licence lets us copy and modify; what it does not let us do is
// drop the attribution, and the two are easy to confuse when the word being
// changed is a name. The rule this file exists to keep is the split:
//
//   - the PRODUCT is ours. Window title, header, empty state, the word a
//     reader sees. Those are here.
//   - the ATTRIBUTION is upstream's. Copyright headers on vendored files,
//     `upstream/LICENSE`, and `NOTICE.md` say the conversation view is
//     DeepSeek's work under the MIT licence, and nothing in this file or any
//     other may edit, weaken or paraphrase them.
//
// A string that names DeepSeek because it is *describing provenance* is
// attribution and stays. A string that names DeepSeek because it is *what the
// reader calls this program* is product and is rebranded.
//
// Two vendored files carry a product string in their markup rather than in a
// dictionary - `ChatView.tsx`'s running-turn label and `BrandWordmark.tsx`'s
// name. Those are edited in place, and each says so in its own header, which
// is what the licence asks of a modified copy. Everything else a reader sees
// comes from here or from `locale.ts`.

/** What a reader calls this program. */
export const PRODUCT = 'tetanus'

/** The browser tab. */
export const TITLE = 'tetanus panel'

/** The line under the wordmark on an empty conversation. */
export const TAGLINE = 'a harness you can read'
