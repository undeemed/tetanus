//! The bridge: a server's tools, in the tetanus registry, dispatched by the
//! same pipeline as the native ones.
//!
//! **The raw name goes on the wire; the public name goes to the model.** Two
//! servers may both advertise `search`, and so may a tetanus tool, so a
//! server's tools are published as `mcp__<server>__<raw>`. Normalising that to
//! what a provider accepts as a function name is lossy, so the pair is carried
//! on the tool rather than recovered by parsing the public name back - a
//! parser would be wrong exactly for the names that needed normalising.
//!
//! **An MCP call is exclusive.** [`tetanus_turn::tools::ToolMode`] is opt-in
//! for a reason: a tool says it may overlap only for arguments it has looked
//! at. Nothing here has looked at anything - the body is a program this
//! process did not write, doing work it did not describe - so every MCP call
//! is a barrier. That is a cost, and it is the cost of not guessing.
//!
//! **A failure is a result, with its class in it.** Everything that can go
//! wrong out there comes back as a failed tool result whose text opens with
//! `[class]`, so the model reads a bounded failure and an operator reading the
//! journal can tell a timeout from a server that is gone without parsing
//! prose a server author wrote.

use std::sync::Arc;

use serde_json::Value;
use tetanus_turn::tools::{Tool, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema};

use crate::client::ToolDescription;
use crate::supervisor::Supervisor;

/// The prefix every bridged tool's public name carries, so a reader of a
/// catalogue can tell at a glance which tools are not this harness's own.
pub const NAMESPACE: &str = "mcp";

/// Longest function name a provider accepts. A wire-protocol constant, not
/// configuration: DeepSeek and OpenAI both cap at 64 characters over
/// `[A-Za-z0-9_-]`.
pub const MAX_NAME: usize = 64;

/// Hex digits of the identity hash appended when normalisation loses
/// something.
const HASH_DIGITS: usize = 12;

/// The name the model is offered for one server's tool.
///
/// Clean names join verbatim, which is the case that matters for reading a
/// prompt. A name carrying anything else - a dot, a slash, a space, a
/// non-ASCII letter - or one too long to send, is normalised and given a hash
/// of the identity it came from, so two tools that normalise onto each other
/// still get distinct names.
///
/// One rule is tetanus's rather than upstream's: a *server* whose own name
/// contains the `__` separator is hashed too, however clean it is. Upstream
/// joins it verbatim, which makes the server `a__b` with tool `c` and the
/// server `a` with tool `b__c` one name - reachable there because a server
/// name is whatever a configuration says, and reachable here for the same
/// reason. The cost of closing it is a hash on a name nobody should have
/// written; the cost of leaving it open is one server's tool silently
/// answering for another's.
pub fn public_name(server: &str, raw: &str) -> String {
    let joined = format!("{NAMESPACE}__{server}__{raw}");
    let clean: String = joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean == joined && clean.len() <= MAX_NAME && !server.contains("__") {
        return clean;
    }
    let hash = identity(server, raw);
    let room = MAX_NAME - HASH_DIGITS - 1;
    let head: String = clean.chars().take(room).collect();
    format!("{head}_{hash}")
}

/// A short, stable digest of `(server, raw)`.
///
/// FNV-1a rather than SHA-256, which is upstream's choice: this is a name
/// disambiguator and not a security claim, and the workspace has no hash
/// dependency. What it must be is deterministic across runs and different for
/// different identities, which it is - the unit separator between the two
/// halves is what stops `("ab", "c")` and `("a", "bc")` hashing alike.
fn identity(server: &str, raw: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in server
        .as_bytes()
        .iter()
        .chain(b"\x1f")
        .chain(raw.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")[..HASH_DIGITS].to_string()
}

/// One MCP tool, as the registry and the model see it.
pub struct McpTool {
    supervisor: Arc<Supervisor>,
    /// What the server called it. Only ever sent on the wire.
    raw_name: String,
    /// What the model is offered.
    public_name: String,
    description: String,
    parameters: Value,
}

impl McpTool {
    pub fn new(supervisor: Arc<Supervisor>, described: &ToolDescription) -> Self {
        Self {
            public_name: public_name(supervisor.server(), &described.raw_name),
            raw_name: described.raw_name.clone(),
            description: described.description.clone(),
            parameters: described.input_schema.clone(),
            supervisor,
        }
    }

    pub fn raw_name(&self) -> &str {
        &self.raw_name
    }

    pub fn public_name(&self) -> &str {
        &self.public_name
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.public_name.clone(),
            // The server's own words, with the server named, because a model
            // choosing between two `search` tools has nothing else to go on.
            description: match self.description.trim() {
                "" => format!("A tool of the MCP server {:?}.", self.supervisor.server()),
                said => format!("{said} (MCP server {:?})", self.supervisor.server()),
            },
            parameters: self.parameters.clone(),
        }
    }

    /// Exclusive, always. See the module note: nothing here knows what the
    /// call does, and a barrier is the answer that cannot make things worse.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Exclusive
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        match self.supervisor.call_tool(&self.raw_name, arguments).await {
            Ok(answer) => Ok(ToolOutcome::ok(answer.text)),
            Err(fault) => Err(ToolError::Failed(
                self.public_name.clone(),
                format!("[{}] {fault}", fault.class()),
            )),
        }
    }
}

/// Register every tool a server advertises, and answer with the public names
/// that were added.
///
/// Duplicate public names cannot arise from one server - two tools of the same
/// raw name would be one entry in the server's own list - and across servers
/// the server id is in the name.
pub fn install(
    registry: &mut ToolRegistry,
    supervisor: &Arc<Supervisor>,
    tools: &[ToolDescription],
) -> Vec<String> {
    tools
        .iter()
        .map(|described| {
            let tool = McpTool::new(Arc::clone(supervisor), described);
            let name = tool.public_name().to_string();
            registry.register(Arc::new(tool));
            name
        })
        .collect()
}
