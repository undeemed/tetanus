//! Who a model request says it is.
//!
//! A provider that has to tell one client from another reads the `User-Agent`;
//! a harness that sends none is anonymous traffic on somebody's quota, and a
//! provider that wants to warn its users about a bad release has nobody to
//! warn. The identity is static public product facts and nothing else: no
//! user, no machine, no session. That is what makes it safe to send on every
//! request, to every provider, without asking.

use std::collections::BTreeMap;

/// The product name every request is sent under. The binary's name, because
/// that is what a provider's operator would search for.
pub const PRODUCT: &str = "tetanus";

/// Where a provider's operator reads what this product is.
pub const URL: &str = "https://github.com/undeemed/tetanus";

/// The header attribution travels in. Lower case, because that is the form
/// the wire uses and the form a map compares in.
pub const USER_AGENT: &str = "user-agent";

/// The product a request is sent on behalf of.
///
/// A fork sends its own: the point of the header is to name who is calling,
/// so a build that is not this one must be able to say so. `Default` is this
/// product, and the only identity anything in tetanus constructs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIdentity {
    pub product: String,
    pub version: String,
    pub url: String,
}

impl Default for AppIdentity {
    fn default() -> Self {
        Self {
            product: PRODUCT.to_string(),
            // Read from the manifest, never hand-copied: a version this file
            // spelled out would go stale at the next release and misreport
            // which build a provider is talking to.
            version: env!("CARGO_PKG_VERSION").to_string(),
            url: URL.to_string(),
        }
    }
}

/// `product/version (+url)`, the form RFC 9110 gives a product token and its
/// comment.
pub fn user_agent(identity: &AppIdentity) -> String {
    format!(
        "{}/{} (+{})",
        identity.product, identity.version, identity.url
    )
}

/// The provider-neutral baseline: the `User-Agent` and nothing else.
///
/// It is a map rather than one string because attribution is a header set,
/// and a provider that wants its own attribution header adds it here rather
/// than in the transport that sends it.
pub fn attribution_headers(identity: &AppIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([(USER_AGENT.to_string(), user_agent(identity))])
}
