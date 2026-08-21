//! The decision seam: whether one tool call may run, who is asked, and what
//! the journal records about it.
//!
//! Contract section 4.4.7 froze this shape; this serves the engine half of it.
//! Some tools do something a session cannot take back, and until now the only
//! protection a deployment had was not registering the tool.
//!
//! Three rules shape everything here, and each one is a defect if it is
//! quietly relaxed.
//!
//! **The seam fails closed.** Every way of not getting an answer denies. A
//! grant is one specific word from an answerer that ran and returned; anything
//! else - nobody listening, a panic, a withdrawn question, a word this build
//! does not know - is [`ApprovalOutcome::Unavailable`] or
//! [`ApprovalOutcome::Cancelled`], and neither grants.
//!
//! **The policy is decided before the dispatch, not inside it.** A `never`
//! policy is applied by [`ApprovalService::request`] itself rather than by a
//! listener the service registers, because a listener registered later could
//! be ordered ahead of a gate listener and answer first. Upstream makes the
//! same choice for the same reason (`packages/interaction/user-approval`), and
//! its `prepend` cases are what pin it.
//!
//! **The audit pair is one to one.** Every ask appends `approval/asked` before
//! the question goes out and exactly one `approval/decided` once the outcome is
//! known, sharing an `id`. A `never` policy still writes both, so the journal
//! says a question was put and refused rather than saying nothing happened.
//!
//! Parity: upstream `packages/interaction/user-approval`, pinned by its
//! `approval.spec.ts` and `invariant.spec.ts`.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::json;
use tetanus_core::events::{DispatchMode, Event};
use tetanus_core::EventBus;
use tetanus_session::{SessionError, SessionEvent, SessionLog};

use crate::interrupt::Interrupt;
use crate::log::topic;

/// How one approval question settled.
///
/// Contract section 4.4.7. The wire form is
/// [`tetanus_protocol::types::ApprovalOutcome`]; this is the engine's own, and
/// section 7.6 of the contract says why the two are separate types rather than
/// one re-export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Run this call, and only this call. The one grant.
    AllowedOnce,
    /// A decision not to run it.
    Rejected,
    /// The question was withdrawn before it was answered.
    Cancelled,
    /// Nobody could answer it. The fail-closed outcome.
    Unavailable,
}

impl ApprovalOutcome {
    /// Whether this outcome lets the call run.
    ///
    /// Only [`AllowedOnce`](Self::AllowedOnce) does. This exists so no caller
    /// writes the rule as a match of its own: forgetting an arm in a match
    /// that decides permission opens a gate, and forgetting to call this does
    /// not compile.
    pub fn grants(self) -> bool {
        matches!(self, Self::AllowedOnce)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }

    /// Read an outcome that came from outside this build.
    ///
    /// A word this build does not know reads as [`Unavailable`](Self::Unavailable)
    /// rather than as a parse failure. The contract's wire enum keeps the
    /// unknown word so a transcript can record what was actually said; the
    /// engine only has to decide, and a word it cannot interpret is not a
    /// grant.
    pub fn parse(word: &str) -> Self {
        match word {
            "allowed-once" => Self::AllowedOnce,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            _ => Self::Unavailable,
        }
    }
}

/// What happens to an approval question before any answerer sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Put the question to the answerers. The default: a deployment that says
    /// nothing gets the seam, not a bypass of it.
    #[default]
    Ask,
    /// Put it to nobody: every ask settles [`ApprovalOutcome::Rejected`].
    Never,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    /// Read a policy a caller named.
    ///
    /// Unlike [`ApprovalOutcome::parse`] an unknown word is refused rather
    /// than defaulted: a policy is set by a caller that could have named one of
    /// the two, so guessing would hide the mistake instead of reporting it.
    pub fn parse(word: &str) -> Result<Self, ApprovalError> {
        match word {
            "ask" => Ok(Self::Ask),
            "never" => Ok(Self::Never),
            other => Err(ApprovalError::UnknownPolicy(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    /// The audit pair must be enclosed by a turn, because the turn is the unit
    /// crash repair closes (contract sections 4.4.4 and 4.4.7). A question
    /// written between turns is inside nothing, so no repair would ever reach
    /// it and the journal would carry it unanswered for the rest of its life.
    #[error(
        "approval asked outside an open turn: the approval/asked and approval/decided pair must \
         be enclosed by the turn that needs the decision, because a turn is what crash repair \
         closes"
    )]
    NoOpenTurn,
    #[error("approval policy must be \"ask\" or \"never\", not {0:?}")]
    UnknownPolicy(String),
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// One question, as the asker puts it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    /// The tool the question is about.
    pub tool_name: String,
    /// The `tool/call.id` being decided, when the asker has one.
    pub call_id: Option<String>,
    /// The asker's own words for why it is asking. Text for a human, not a
    /// code to match on.
    pub reason: Option<String>,
}

impl ApprovalRequest {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            call_id: None,
            reason: None,
        }
    }

    pub fn about_call(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn because(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// `approval/request` is the answerer seam: a listener decides one question,
/// or delegates down the chain to the fail-closed default.
///
/// The terminal answers [`ApprovalOutcome::Unavailable`], so a bus with no
/// listener denies rather than hanging or granting. That is the whole of what
/// "no answerer" means here.
pub struct ApprovalAsk {
    /// The id this question is audited under, so an answerer that renders a
    /// prompt can name the journal line it belongs to.
    pub id: String,
    pub request: ApprovalRequest,
}

impl Event for ApprovalAsk {
    const TOPIC: &'static str = "approval/request";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = ApprovalOutcome;
}

/// The session's approval policy: the last `approval/policy` on the journal,
/// or `None` when it never switched.
///
/// The fold is the whole state. A resumed session is under the policy it was
/// under with nothing to replay but the log, which is why the switch is a
/// durable event rather than a field on anything.
pub fn effective_policy(events: &[SessionEvent]) -> Option<ApprovalPolicy> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::APPROVAL_POLICY)
        .and_then(|event| event.data["policy"].as_str())
        .and_then(|word| ApprovalPolicy::parse(word).ok())
}

/// Whether the log currently sits inside an open turn.
///
/// Read backwards, so the answer is about the tail and not about whether the
/// journal ever held a turn: the last boundary decides.
pub fn has_open_turn(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|event| match event.ty.as_str() {
            topic::TURN_START => Some(true),
            topic::TURN_END => Some(false),
            _ => None,
        })
        == Some(true)
}

/// Write the durable form of a policy switch.
///
/// Refused before the log changes when the word is not one of the two, so a
/// journal never carries a policy nothing can read back.
pub fn set_policy(
    log: &dyn SessionLog,
    policy: ApprovalPolicy,
) -> Result<SessionEvent, SessionError> {
    log.append(topic::APPROVAL_POLICY, json!({ "policy": policy.as_str() }))
}

/// Asks whether one tool call may run, applies the session's policy, and
/// records both halves of every question on the journal.
pub struct ApprovalService {
    bus: EventBus,
    log: Arc<dyn SessionLog>,
    /// The deployment's policy for a session whose journal holds no switch.
    default_policy: ApprovalPolicy,
    /// Makes each id fresh. Seeded past the asks the journal already carries,
    /// so a resumed session cannot mint an id its own log already uses - the
    /// pair is matched by id, and a reused one would make two questions read as
    /// one.
    next: AtomicU64,
    minted: u64,
}

impl ApprovalService {
    pub fn new(
        bus: EventBus,
        log: Arc<dyn SessionLog>,
        default_policy: ApprovalPolicy,
    ) -> Arc<Self> {
        let already = log
            .events()
            .iter()
            .filter(|event| event.ty == topic::APPROVAL_ASKED)
            .count() as u64;
        let minted = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();
        Arc::new(Self {
            bus,
            log,
            default_policy,
            next: AtomicU64::new(already),
            minted,
        })
    }

    /// The policy this session's asks resolve under right now: its own switch,
    /// else the deployment default.
    pub fn policy(&self) -> ApprovalPolicy {
        effective_policy(&self.log.events()).unwrap_or(self.default_policy)
    }

    /// The session's own switch, without applying the deployment default.
    pub fn override_of(&self) -> Option<ApprovalPolicy> {
        effective_policy(&self.log.events())
    }

    /// Switch this session's policy. Writing the policy it is already under
    /// appends nothing, so a caller may send it idempotently.
    ///
    /// Answers whether the journal was written.
    pub fn set_policy(&self, policy: ApprovalPolicy) -> Result<bool, ApprovalError> {
        if self.policy() == policy {
            return Ok(false);
        }
        set_policy(self.log.as_ref(), policy)?;
        Ok(true)
    }

    /// Put one question, and record it.
    ///
    /// The order is fixed and is the contract's: the ask is durable *before*
    /// anyone is asked, and the decision is durable before this returns. A
    /// caller therefore cannot act on an outcome the journal does not carry.
    ///
    /// Every path through this produces an outcome. The only error is a caller
    /// mistake - asking with no turn open - and it is raised before anything is
    /// appended, so a refused ask leaves no half of a pair behind.
    pub async fn request(
        &self,
        request: ApprovalRequest,
        interrupt: &Interrupt,
    ) -> Result<ApprovalOutcome, ApprovalError> {
        if !has_open_turn(&self.log.events()) {
            return Err(ApprovalError::NoOpenTurn);
        }

        let id = self.fresh_id();
        let mut asked = json!({ "id": id, "tool_name": request.tool_name });
        if let Some(call_id) = &request.call_id {
            asked["call_id"] = json!(call_id);
        }
        if let Some(reason) = &request.reason {
            asked["reason"] = json!(reason);
        }
        self.log.append(topic::APPROVAL_ASKED, asked)?;

        let outcome = self.decide(id.clone(), request, interrupt).await;

        self.log.append(
            topic::APPROVAL_DECIDED,
            json!({ "id": id, "outcome": outcome.as_str() }),
        )?;
        Ok(outcome)
    }

    /// Settle one question: the withdrawal check, then the policy gate, then
    /// the answerers.
    async fn decide(
        &self,
        id: String,
        request: ApprovalRequest,
        interrupt: &Interrupt,
    ) -> ApprovalOutcome {
        // A question nobody is waiting for any more is withdrawn rather than
        // put, so an interrupt that already landed costs no round trip.
        if interrupt.stopped() {
            return ApprovalOutcome::Cancelled;
        }
        // Before the dispatch, deliberately: see the module note. A `never`
        // policy that were a listener could be answered ahead of by a listener
        // registered later, and "never" would then be a default rather than a
        // guarantee.
        if self.policy() == ApprovalPolicy::Never {
            return ApprovalOutcome::Rejected;
        }

        let mut ask = ApprovalAsk { id, request };
        let answered = self.bus.waterfall(&mut ask, fail_closed());

        // The interrupt withdraws the question. A late answer is discarded by
        // construction: this returns without it, and the journal already has
        // the decision by the time anything could observe one.
        tokio::select! {
            biased;
            outcome = contained(answered) => outcome,
            _ = interrupt.cancelled() => ApprovalOutcome::Cancelled,
        }
    }

    fn fresh_id(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        format!("ask-{}-{n}", self.minted)
    }
}

/// The terminal of the answerer chain: nobody answered, so nobody granted.
fn fail_closed() -> tetanus_core::events::Terminal<ApprovalAsk> {
    Arc::new(|_ask: &mut ApprovalAsk| Box::pin(async move { ApprovalOutcome::Unavailable }))
}

/// Contain an answerer that panics as the fail-closed outcome.
///
/// The bus keeps `waterfall` loud on purpose, because a decision listener that
/// panics is a bug its caller should see. This seam is the exception the
/// contract requires: a question that cannot be answered has a defined answer,
/// and letting the panic unwind would fail the *turn* instead of denying the
/// *call* - which is a worse outcome than the denial, and a less safe one.
async fn contained(body: impl std::future::Future<Output = ApprovalOutcome>) -> ApprovalOutcome {
    match AssertUnwindSafe(body).catch_unwind().await {
        Ok(outcome) => outcome,
        Err(payload) => {
            let fault = crate::tools::panicked(payload);
            tracing::error!(%fault, "an approval answerer panicked");
            ApprovalOutcome::Unavailable
        }
    }
}
