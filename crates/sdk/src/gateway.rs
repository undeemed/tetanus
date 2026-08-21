//! The request surface as data: what calls exist, what arguments they take,
//! and one way in that checks both before dispatching.
//!
//! Upstream's gateway resolves a generated descriptor per call, validates the
//! exact named arguments against it, invokes the business method, and validates
//! the result. tetanus already has the invoking part - `crates/rpc`'s codec
//! does it - but the codec's `match` over method names is unreachable to a
//! caller: nothing in this workspace can be *asked* what the calls are. That is
//! what a descriptor catalog is for, and it is not a convenience. A routing arm
//! is written by hand, so the arm that gets forgotten is the one no case names,
//! and [`DESCRIPTORS`] is a list a case can iterate.
//!
//! Restated, not transcribed. There is no code generation here and no type
//! projection: `crates/protocol` already holds every params type, so a
//! descriptor names the fields and `serde` checks the values, which is the same
//! validation done by the component that already owns it.
//!
//! Unary calls only, as upstream's is. `session.subscribe` needs somewhere to
//! push, and a subscription opened into a sink nobody reads is worse than a
//! refusal, so [`Gateway::invoke`] refuses it by name and
//! [`Gateway::invoke_streaming`] is where a caller that has a sink goes.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tetanus_protocol::methods::{capability, method, Engine, EventSink};
use tetanus_protocol::rpc::{ErrorCode, RpcError};

/// One named argument of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: &'static str,
    /// False for a field the contract marks optional. An absent optional
    /// argument is not the same as one sent as `null`, and neither is an error.
    pub required: bool,
}

impl ParamSpec {
    const fn required(name: &'static str) -> Self {
        Self {
            name,
            required: true,
        }
    }

    const fn optional(name: &'static str) -> Self {
        Self {
            name,
            required: false,
        }
    }
}

/// Everything a caller needs to make one call without reading the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvocationDescriptor {
    pub endpoint: &'static str,
    pub params: &'static [ParamSpec],
    /// The `capability` string a server advertises when it serves this call,
    /// for the calls contract section 4.4.1 makes optional. `None` means every
    /// build serves it.
    pub capability: Option<&'static str>,
    /// True for a call contract section 4.2 reserves: routed, and answered
    /// `NotImplemented` by a build that does not serve it. Described anyway,
    /// because a caller meeting `MethodNotFound` for a reserved call would
    /// conclude the contract does not have it.
    pub reserved: bool,
    /// True for a call that needs somewhere to push. Refused by
    /// [`Gateway::invoke`], served by [`Gateway::invoke_streaming`].
    pub streaming: bool,
}

impl InvocationDescriptor {
    const fn call(endpoint: &'static str, params: &'static [ParamSpec]) -> Self {
        Self {
            endpoint,
            params,
            capability: None,
            reserved: false,
            streaming: false,
        }
    }

    const fn optional(self, capability: &'static str) -> Self {
        Self {
            capability: Some(capability),
            ..self
        }
    }

    const fn reserved(self) -> Self {
        Self {
            reserved: true,
            ..self
        }
    }

    const fn streaming(self) -> Self {
        Self {
            streaming: true,
            ..self
        }
    }

    /// Whether an argument by this name belongs to this call.
    pub fn accepts(&self, name: &str) -> bool {
        self.params.iter().any(|param| param.name == name)
    }
}

const NONE: &[ParamSpec] = &[];

/// Every call this build routes, described.
///
/// Ordered as contract section 4.4 orders them, so the catalog reads as the
/// contract's own table of contents rather than as an alphabetised list.
pub const DESCRIPTORS: &[InvocationDescriptor] = &[
    InvocationDescriptor::call(
        method::HELLO,
        &[
            ParamSpec::required("protocol_version"),
            ParamSpec::required("client"),
        ],
    ),
    InvocationDescriptor::call(
        method::SESSION_CREATE,
        &[
            ParamSpec::optional("session_id"),
            ParamSpec::optional("path"),
            ParamSpec::optional("provider"),
            ParamSpec::optional("model"),
            ParamSpec::optional("max_steps"),
        ],
    ),
    InvocationDescriptor::call(method::SESSION_LIST, NONE),
    InvocationDescriptor::call(
        method::SESSION_EVENTS,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::optional("from_seq"),
            ParamSpec::optional("limit"),
        ],
    ),
    InvocationDescriptor::call(
        method::SESSION_FORK,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::optional("through_seq"),
            ParamSpec::optional("child_session_id"),
        ],
    )
    .optional(capability::SESSION_FORK),
    InvocationDescriptor::call(
        method::SESSION_SUBSCRIBE,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::optional("from_seq"),
        ],
    )
    .optional(capability::SESSION_SUBSCRIBE)
    .streaming(),
    InvocationDescriptor::call(
        method::SESSION_UNSUBSCRIBE,
        &[ParamSpec::required("subscription_id")],
    )
    .optional(capability::SESSION_SUBSCRIBE),
    InvocationDescriptor::call(
        method::AGENT_PROMPT,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::required("content"),
        ],
    ),
    InvocationDescriptor::call(method::AGENT_STATUS, &[ParamSpec::required("session_id")]),
    InvocationDescriptor::call(
        method::AGENT_INTERRUPT,
        &[ParamSpec::required("session_id")],
    )
    .optional(capability::AGENT_INTERRUPT),
    InvocationDescriptor::call(
        method::AGENT_STEER,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::required("content"),
        ],
    )
    .optional(capability::AGENT_STEER)
    .reserved(),
    InvocationDescriptor::call(method::CATALOG_TOOLS, NONE),
    InvocationDescriptor::call(method::CATALOG_MODELS, NONE),
    InvocationDescriptor::call(method::CONFIG_DUMP, NONE),
    InvocationDescriptor::call(
        method::APPROVAL_SET,
        &[
            ParamSpec::required("session_id"),
            ParamSpec::required("policy"),
        ],
    )
    .optional(capability::APPROVAL_SET)
    .reserved(),
];

/// The descriptor for one endpoint, or `None` for a name no contract call has.
pub fn describe(endpoint: &str) -> Option<&'static InvocationDescriptor> {
    DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.endpoint == endpoint)
}

/// Dispatch by endpoint name and named arguments.
///
/// Holds an engine and nothing else: no session, no connection, no handshake
/// state. The handshake belongs to a connection, and this is not one - a
/// caller that wants the rule enforced uses [`crate::Client`], which is where
/// per-connection state lives.
pub struct Gateway {
    engine: Arc<dyn Engine>,
}

impl Gateway {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self { engine }
    }

    /// Every call, described. The same list for every build, because a
    /// descriptor says what the *contract* has; whether this build serves an
    /// optional one is [`InvocationDescriptor::capability`] and the handshake.
    pub fn descriptors(&self) -> &'static [InvocationDescriptor] {
        DESCRIPTORS
    }

    /// Make one unary call.
    ///
    /// The arguments are named, exactly: a missing required one and an
    /// unrecognised one both fail before the engine is touched, each naming
    /// the field at fault in `data.field`. Accepting an unrecognised argument
    /// silently is the failure mode worth spending a check on - it turns a
    /// caller's typo into a call that quietly did something else.
    pub async fn invoke(
        &self,
        endpoint: &str,
        args: Map<String, Value>,
    ) -> Result<Value, RpcError> {
        let descriptor = self.validate(endpoint, &args)?;
        if descriptor.streaming {
            return Err(RpcError::new(
                ErrorCode::InvalidRequest,
                format!("`{endpoint}` delivers pushes and needs a sink; use `invoke_streaming`"),
            )
            .with_data(serde_json::json!({ "method": endpoint })));
        }
        let sink: Arc<dyn EventSink> = Arc::new(Discard);
        self.dispatch(endpoint, Value::Object(args), sink).await
    }

    /// Make any call, including one that pushes.
    pub async fn invoke_streaming(
        &self,
        endpoint: &str,
        args: Map<String, Value>,
        sink: Arc<dyn EventSink>,
    ) -> Result<Value, RpcError> {
        self.validate(endpoint, &args)?;
        self.dispatch(endpoint, Value::Object(args), sink).await
    }

    fn validate(
        &self,
        endpoint: &str,
        args: &Map<String, Value>,
    ) -> Result<&'static InvocationDescriptor, RpcError> {
        let Some(descriptor) = describe(endpoint) else {
            return Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("no endpoint `{endpoint}`"),
            )
            .with_data(serde_json::json!({ "method": endpoint })));
        };
        for param in descriptor.params.iter().filter(|param| param.required) {
            if !args.contains_key(param.name) {
                return Err(field_error(
                    param.name,
                    format!("`{endpoint}` requires `{}`", param.name),
                ));
            }
        }
        for name in args.keys() {
            if !descriptor.accepts(name) {
                return Err(field_error(
                    name,
                    format!("`{endpoint}` has no argument `{name}`"),
                ));
            }
        }
        Ok(descriptor)
    }

    async fn dispatch(
        &self,
        endpoint: &str,
        params: Value,
        sink: Arc<dyn EventSink>,
    ) -> Result<Value, RpcError> {
        let engine = &self.engine;
        match endpoint {
            method::HELLO => encode(engine.hello(typed(params)?).await?),
            method::SESSION_CREATE => encode(engine.session_create(typed(params)?).await?),
            method::SESSION_LIST => encode(engine.session_list().await?),
            method::SESSION_EVENTS => encode(engine.session_events(typed(params)?).await?),
            method::SESSION_FORK => encode(engine.session_fork(typed(params)?).await?),
            method::SESSION_SUBSCRIBE => {
                encode(engine.session_subscribe(typed(params)?, sink).await?)
            }
            method::SESSION_UNSUBSCRIBE => {
                encode(engine.session_unsubscribe(typed(params)?).await?)
            }
            method::AGENT_PROMPT => encode(engine.agent_prompt(typed(params)?).await?),
            method::AGENT_STATUS => encode(engine.agent_status(typed(params)?).await?),
            method::AGENT_INTERRUPT => encode(engine.agent_interrupt(typed(params)?).await?),
            method::AGENT_STEER => encode(engine.agent_steer(typed(params)?).await?),
            method::CATALOG_TOOLS => encode(engine.catalog_tools().await?),
            method::CATALOG_MODELS => encode(engine.catalog_models().await?),
            method::CONFIG_DUMP => encode(engine.config_dump().await?),
            method::APPROVAL_SET => encode(engine.approval_set(typed(params)?).await?),
            // Unreachable while `validate` runs first: an endpoint with a
            // descriptor and no arm is the mistake the catalog exists to catch,
            // and it is reported rather than panicked on.
            unknown => Err(RpcError::new(
                ErrorCode::NotImplemented,
                format!("`{unknown}` is described but this build routes it nowhere"),
            )
            .with_data(serde_json::json!({ "method": unknown }))),
        }
    }
}

/// The sink a unary call gets: one that throws away what it is given.
///
/// A unary call never pushes, so nothing reaches this. It exists so
/// `session_subscribe`'s signature can be honoured on a path that has refused
/// to call it, rather than by making the argument optional everywhere.
struct Discard;

impl EventSink for Discard {
    fn session_event(&self, _: tetanus_protocol::methods::SessionEventPush) {}
    fn agent_status(&self, _: tetanus_protocol::methods::AgentStatusPush) {}
}

fn field_error(field: &str, message: String) -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, message)
        .with_data(serde_json::json!({ "field": field }))
}

/// The value check, after the name check.
///
/// Names are the descriptor's business and values are `serde`'s, and the split
/// is why there is no schema here: `crates/protocol` already spells every
/// params type, and a second description of the same shape would be a second
/// thing to keep in step.
///
/// It is also why `data.field` is present for a name fault and absent for a
/// value fault. `serde` names the key for a missing or unknown field and
/// describes the mismatch for a wrong value - "invalid type: string, expected
/// u64" - without saying where. Guessing which argument it meant would put a
/// field name a surface renders as fact behind an inference, so a value fault
/// carries `serde`'s own words and no `field`. Contract section 4.5 promises
/// `data.field` "when one field is at fault", not always, and this is which
/// half is which.
fn typed<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|error| {
        let message = error.to_string();
        match field_at_fault(&message).map(str::to_string) {
            Some(field) => field_error(&field, message),
            None => RpcError::new(ErrorCode::InvalidParams, message),
        }
    })
}

/// The one field `serde` named, when it named one.
fn field_at_fault(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once('`')?;
    let (field, _) = rest.split_once('`')?;
    (!field.is_empty()).then_some(field)
}

fn encode<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))
}
