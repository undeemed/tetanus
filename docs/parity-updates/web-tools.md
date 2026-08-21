# Parity update: the web tools (slice `web`)

Not folded into [`../parity.md`](../parity.md) by this branch; the
reconciliation slice folds every lane's note in one pass.

## 1. Section 3, the `web/*` row

Replaces the row reading `| `web/*` (fetch, search providers) | 11 | None | Web fetch and search tools | ② |`.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `web/*` (fetch, search providers) | 11 | `web_fetch` and `web_search`, both above one transport seam so the whole policy is asserted offline: a URL judged before anything is sent, same-origin redirects followed under a hop cap, a declared length past the byte cap refused before the body is read and an undeclared one cut at it, a short content-type list, two charsets, and truncation stated in what the model reads. Search is a provider trait with a registry that refuses to choose between two usable providers, one real provider (DeepSeek's Messages search) mapped over the same seam, and a deterministic mock | HTML converted to markdown rather than stripped, more providers (Exa, Perplexity), a credential service so a key can rotate without a restart, spill files for a large body, and the presentation metadata upstream's cards read | ② |

## 2. Section 4, the port table

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `web/web-fetch-http/tests/fetch-http.spec.ts` | `crates/web/tests/upstream_fetch.rs` | Fetching one page under stated limits | part ported: TC-PORT-WEB-1..13 for classification and the user agent, a declared length past the cap refused before the body, three truncation cases including the exact fill, an unsupported type and no type at all, a declared charset decoded and an unknown one refused, absolute and relative same-origin redirects, a cross-origin hop and a credentialled hop both blocked with nothing sent, the hop cap asserted exactly (n hops pass, n+1 requests then blocked, a cap of zero still fetches), a redirect with no Location, a non-2xx as a result, transport failures passing their code through, and markup stripped with script, style and comments dropped. Upstream converts HTML to markdown with turndown and a large part of its suite is that library's output - tables, entity tables, a depth preflight against pathological nesting - which has nothing to restate against a stripper; TC-PORT-WEB-13 pins what the stripper does promise, including that an unterminated tag terminates. Its abort-signal cases have no counterpart: a tetanus tool call is bounded by a timeout, not by a caller's signal |
| `web/web/tests/web.spec.ts`, `web-search-deepseek/tests/deepseek.spec.ts`, `tool-web/tests/tool-web.spec.ts` | `crates/web/tests/upstream_search.rs` | Choosing a search provider, one provider's wire format, and what the model reads | part ported: TC-PORT-WEB-14..26 for the five resolution outcomes (nothing registered, nothing usable, ambiguous with both candidates named, configured-missing, configured-unavailable) plus a refused duplicate id, the runtime's result cap and its truncation flag, the DeepSeek provider unusable until it is configured, the request it posts, the answer it maps - citations joined to results, `page_age` carried unparsed, repeated URLs deduplicated first-wins, empty URLs skipped - every failure shape as `WEB_PROVIDER_ERROR` including strict mode's prose-only refusal, and the two tools: rendered sources with hostnames and a citation reminder, an unparseable URL as its own label, empty arguments refused before a provider is asked, and a failure reaching the model as a failed call carrying its code. Upstream's Exa and Perplexity providers are two more implementations of the same trait. Its credential-resolver races need a credential service this build does not have; TC-PORT-WEB-17 pins the part that survives - a missing key makes a provider unusable rather than making a search fail. Its presentation metadata and card views serve a surface contract this crate does not |

## 3. Changelog row

| 2026-08-21 | The web tools implemented (`crates/web`, TC-PORT-WEB-1..26), opening the `web/*` row. Both tools sit above one transport seam, and that is the design decision rather than a testing convenience: every rule worth having - the scheme, the redirect, the size, the type, the charset - is a decision made above the socket, so the whole policy is asserted with no network in the suite and the live transport is thin enough to read in one sitting. Three rules have teeth. A URL carrying credentials is never sent, and neither is a redirect to one, because `https://user:token@host/` in a model's output is a credential about to be handed to whoever answers. A hop that leaves the origin is refused, because a page that redirects to an internal address is how a fetch tool becomes a request forgery - TC-PORT-WEB-7 asserts the blocked destination was never requested, not merely that the call failed. And truncation is always stated in the text the model reads, because a model given half a page and no notice answers confidently about the half it did not get. Search resolves rather than picks: two usable providers and no configured choice is a refusal naming both, since answering the same query from a different engine per run is not something anyone can debug from a journal. One provider is real and mapped over the same seam, with upstream's strict mode kept - an answer with no result block is refused, because an uncited paragraph presented as search results is how a citation becomes a hallucination. |

---

# Parity update: turning the web tools on (slice `web-settings`)

## 1. Section 3, the `web/*` row

Append to `Today`: ", both registered from the settings document under `web.tools.*`, with the fetch limits, the search provider and its credential read from the same document and refused there when they cannot be run".

## 2. Section 4, the port table

| Upstream file | tetanus case file | What it pins | Status |
| --- | --- | --- | --- |
| `web/tool-web/tests/tool-web.spec.ts` (registration), `web-search-deepseek/tests/settings.spec.ts` | `crates/web/tests/settings.rs` | Which web tools a document turns on, and with what | part ported: TC-PORT-WEB-27..31 for both tools off unless the document says otherwise, `web_search` registered whether or not a provider can serve, the fetch limits read and every impossible one refused where it is written (with a hop cap of zero accepted, because following no redirect is a thing to ask for), the credential read from the document or from the environment behind it with a blank value counting as absent, and a configured provider this build does not carry refused when a search resolves. One difference is deliberate and TC-PORT-WEB-27 states it: upstream registers both tools by default, because loading its plugin is already the deployment's choice, while a tetanus registry is compiled in - so a harness whose first run in a sandbox quietly fetched a URL a model invented would be a surprise nobody asked for. Its per-tool timeout budget belongs to a tool-call scheduler this build does not have |
