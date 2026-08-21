//! What can go wrong out on the web, with the code upstream gives it.
//!
//! The codes are upstream's strings rather than tetanus inventions, because a
//! deployment that reads one in a journal and searches for it should land on
//! the same meaning in either system. [`WebFault::code`] is what a failed tool
//! result leads with.

/// Every code [`WebFault::code`] can answer.
pub mod code {
    pub const INVALID_ARGS: &str = "INVALID_ARGS";
    pub const BAD_URL: &str = "WEB_BAD_URL";
    pub const TOO_LARGE: &str = "WEB_FETCH_TOO_LARGE";
    pub const UNSUPPORTED_TYPE: &str = "WEB_UNSUPPORTED_CONTENT_TYPE";
    pub const UNSUPPORTED_CHARSET: &str = "WEB_UNSUPPORTED_CHARSET";
    pub const REDIRECT_BLOCKED: &str = "WEB_REDIRECT_BLOCKED";
    pub const TIMEOUT: &str = "WEB_FETCH_TIMEOUT";
    pub const PROVIDER_ERROR: &str = "WEB_PROVIDER_ERROR";
    pub const PROVIDER_UNAVAILABLE: &str = "WEB_PROVIDER_UNAVAILABLE";
    pub const PROVIDER_AMBIGUOUS: &str = "WEB_PROVIDER_AMBIGUOUS";
    pub const CONFIGURED_MISSING: &str = "WEB_PROVIDER_CONFIGURED_MISSING";
    pub const CONFIGURED_UNAVAILABLE: &str = "WEB_PROVIDER_CONFIGURED_UNAVAILABLE";
    pub const DUPLICATE_PROVIDER: &str = "WEB_DUPLICATE_PROVIDER";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebFault {
    #[error("{0}")]
    InvalidArguments(String),

    /// A URL this tool will not send: a scheme that is not http, a host that
    /// is missing, or credentials somebody put in the authority.
    #[error("{0}")]
    BadUrl(String),

    #[error("the page is larger than the {limit} byte cap this fetch runs under{}", stated(.declared))]
    TooLarge { limit: usize, declared: Option<u64> },

    #[error("this fetch reads text, HTML, markdown and JSON, not {0:?}")]
    UnsupportedType(String),

    #[error("this fetch decodes UTF-8 and ISO-8859-1, not {0:?}")]
    UnsupportedCharset(String),

    /// A redirect that was refused, either because it left the origin or
    /// because there were too many. Upstream reports both as
    /// `WEB_REDIRECT_BLOCKED` and says which in the message.
    #[error("{0}")]
    RedirectBlocked(String),

    #[error("the server did not answer within the budget for this fetch")]
    Timeout,

    #[error("{0}")]
    Provider(String),

    #[error("no web {capability} provider is registered and usable")]
    ProviderUnavailable { capability: String },

    #[error("more than one usable {capability} provider is registered ({}) and none is configured; name one", listed(.candidates))]
    ProviderAmbiguous {
        capability: String,
        candidates: Vec<String>,
    },

    #[error("the configured {capability} provider {id:?} is not registered; registered: {}", listed(.registered))]
    ConfiguredMissing {
        capability: String,
        id: String,
        registered: Vec<String>,
    },

    #[error("the configured {capability} provider {id:?} is registered but not usable: {why}")]
    ConfiguredUnavailable {
        capability: String,
        id: String,
        why: String,
    },

    #[error("a {capability} provider called {id:?} is already registered")]
    DuplicateProvider { capability: String, id: String },
}

impl WebFault {
    /// The upstream code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArguments(_) => code::INVALID_ARGS,
            Self::BadUrl(_) => code::BAD_URL,
            Self::TooLarge { .. } => code::TOO_LARGE,
            Self::UnsupportedType(_) => code::UNSUPPORTED_TYPE,
            Self::UnsupportedCharset(_) => code::UNSUPPORTED_CHARSET,
            Self::RedirectBlocked(_) => code::REDIRECT_BLOCKED,
            Self::Timeout => code::TIMEOUT,
            Self::Provider(_) => code::PROVIDER_ERROR,
            Self::ProviderUnavailable { .. } => code::PROVIDER_UNAVAILABLE,
            Self::ProviderAmbiguous { .. } => code::PROVIDER_AMBIGUOUS,
            Self::ConfiguredMissing { .. } => code::CONFIGURED_MISSING,
            Self::ConfiguredUnavailable { .. } => code::CONFIGURED_UNAVAILABLE,
            Self::DuplicateProvider { .. } => code::DUPLICATE_PROVIDER,
        }
    }
}

fn stated(declared: &Option<u64>) -> String {
    match declared {
        Some(bytes) => format!(" (it declared {bytes})"),
        None => String::new(),
    }
}

fn listed(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}
