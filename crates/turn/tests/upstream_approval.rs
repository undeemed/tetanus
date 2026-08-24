//! Test Design Specification: the approval seam, ported.
//!
//! Feature under test: `tetanus_turn::approval` - whether one tool call may
//! run, who is asked, what the session's policy does before anyone is asked,
//! and what the journal records about all of it. Contract section 4.4.7 is the
//! specification; upstream pins the same rules in
//! `packages/interaction/user-approval/tests/approval.spec.ts` and its
//! `invariant.spec.ts`, and each case names the upstream case it comes from.
//!
//! Approach: a real `JsonlSessionLog` in a temp directory with a `turn/start`
//! written to satisfy the enclosure precondition, an `EventBus` for answerers,
//! and assertions against the journal rather than against a return value
//! alone - the pair being durable is half of what this seam promises.
//!
//! What is not restated, and why. Upstream routes questions with Cordis scopes
//! so a UI answers only for agents it owns; tetanus has one bus per session, so
//! its scoped-dispatch, carrier-key and foreign-scope cases have no
//! counterpart - the containment they exist to give is structural here.
//! Upstream also pins that a request object is *borrowed* by identity; a
//! tetanus request is an owned value handed to the event, so identity is
//! unrepresentable and the cases assert the fields instead. Its
//! observer-throws-after-append pair is `crates/core`'s containment
//! (TC-PORT-CONTAIN-1..5), and its HMR disposal case is `EffectHandle`'s
//! (TC-EFFECT-*). The live policy-switch notice it injects as a user message,
//! and the runtime-context sentence it contributes to the system prompt, need
//! surfaces this slice does not build; `docs/parity.md` carries them.
//!
//! Environmental needs: a writable temp directory and a Tokio runtime. No case
//! reaches a network or an API key. Two cases panic on purpose, so the suite
//! drops exactly their payload from the hook and passes every other panic
//! through.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic that escapes the seam.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use serde_json::json;
use tempfile::TempDir;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::approval::{
    effective_policy, has_open_turn, ApprovalAsk, ApprovalError, ApprovalOutcome, ApprovalPolicy,
    ApprovalRequest, ApprovalService,
};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::log::topic;

/// TC-PORT-APPR-1: an ask with no turn open is refused, and writes nothing.
///
/// Upstream: "throws before appending anything when no turn has ever opened"
/// and "throws between turns - a closed turn does not satisfy the enclosure
/// precondition".
///
/// Upstream requires the enclosure because it batches appends and scans back to
/// the last `turn/end`, so a bare event between turns is crash-tail garbage.
/// That reasoning does not restate: tetanus fsyncs one record per append, so a
/// between-turn event is durable. The tetanus reason is repair - the turn is
/// the unit `session.create` balances, so a question outside one could never be
/// closed and would sit unanswered for the life of the journal.
///
/// Input: an ask on a journal that never opened a turn, and one on a journal
/// whose only turn is closed.
/// Expected: `NoOpenTurn` both times, and not one event appended - a refused
/// ask must not leave half a pair behind.
#[tokio::test]
async fn an_ask_outside_an_open_turn_is_refused_and_writes_nothing() {
    for opening in [
        vec![],
        vec![(topic::TURN_START, 1u64), (topic::TURN_END, 1)],
    ] {
        let h = Fixture::bare().await;
        for (ty, turn) in &opening {
            h.log.append(ty, json!({ "turn": turn })).expect("append");
        }
        let before = h.log.events().len();

        let refused = h
            .service()
            .request(ApprovalRequest::new("echo"), &Interrupt::default())
            .await
            .expect_err("an ask outside a turn is a caller mistake");

        assert!(matches!(refused, ApprovalError::NoOpenTurn));
        assert_eq!(h.log.events().len(), before, "nothing was appended");
    }
}

/// TC-PORT-APPR-2: nobody listening is a denial, and the pair is still on the
/// journal.
///
/// Upstream: "fails closed to unavailable when nobody listens, auditing the
/// asked/decided pair".
///
/// Input: an ask carrying a call id and a reason, with no answerer registered.
/// Expected: `unavailable`; exactly the two audit events in order; the ask
/// carries the tool, the call and the reason; the decision carries the outcome
/// and the same `id`. The pairing is by id and never by adjacency, so the id
/// is asserted rather than assumed.
#[tokio::test]
async fn no_answerer_denies_and_still_audits_the_pair() {
    let h = Fixture::in_turn().await;

    let outcome = h
        .service()
        .request(
            ApprovalRequest::new("echo")
                .about_call("call-1")
                .because("hook says ask"),
            &Interrupt::default(),
        )
        .await
        .expect("an ask inside a turn always produces an outcome");

    assert_eq!(outcome, ApprovalOutcome::Unavailable);
    assert!(!outcome.grants(), "the fail-closed outcome is not a grant");

    let audit = h.audit();
    assert_eq!(
        audit.iter().map(|e| e.ty.as_str()).collect::<Vec<_>>(),
        [topic::APPROVAL_ASKED, topic::APPROVAL_DECIDED]
    );
    assert_eq!(audit[0].data["tool_name"], json!("echo"));
    assert_eq!(audit[0].data["call_id"], json!("call-1"));
    assert_eq!(audit[0].data["reason"], json!("hook says ask"));
    assert_eq!(audit[1].data["outcome"], json!("unavailable"));
    assert_eq!(audit[1].data["id"], audit[0].data["id"]);
}

/// TC-PORT-APPR-3: a question with nothing optional to say says nothing.
///
/// Upstream: "omits absent optional fields from the asked audit event".
///
/// Input: an ask with no call id and no reason.
/// Expected: the ask carries `id` and `tool_name` and no other key. An absent
/// field is absent rather than null, so a reader can tell "no call" from "a
/// call named null" - the same rule contract section 4.3.1 applies to every
/// other payload.
#[tokio::test]
async fn an_ask_with_no_call_or_reason_carries_neither_key() {
    let h = Fixture::in_turn().await;

    h.service()
        .request(ApprovalRequest::new("echo"), &Interrupt::default())
        .await
        .expect("outcome");

    let asked = &h.audit()[0];
    let mut keys: Vec<&str> = asked
        .data
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "tool_name"]);
}

/// TC-PORT-APPR-4: the first answerer to return decides.
///
/// Upstream: "returns the first answering listener outcome (single decision
/// slot)" and "lets a non-owning listener delegate via next() down to the
/// fail-closed default".
///
/// Input: an answerer that grants; then, on a second fixture, one that
/// delegates to the chain with nothing behind it.
/// Expected: the grant is the outcome and reaches the journal; the delegation
/// falls through to `unavailable`, so declining to answer is not the same as
/// answering yes.
#[tokio::test]
async fn the_first_answerer_decides_and_delegation_falls_through() {
    let h = Fixture::in_turn().await;
    let _answerer = grants(&h.bus, ApprovalOutcome::AllowedOnce);

    let outcome = h
        .service()
        .request(ApprovalRequest::new("shell"), &Interrupt::default())
        .await
        .expect("outcome");

    assert_eq!(outcome, ApprovalOutcome::AllowedOnce);
    assert!(outcome.grants(), "this is the one word that grants");
    assert_eq!(h.audit()[1].data["outcome"], json!("allowed-once"));

    let h = Fixture::in_turn().await;
    let _delegating = h
        .bus
        .on_waterfall::<ApprovalAsk, _>(|ev, next| Box::pin(next.run(ev)));

    let outcome = h
        .service()
        .request(ApprovalRequest::new("shell"), &Interrupt::default())
        .await
        .expect("outcome");

    assert_eq!(outcome, ApprovalOutcome::Unavailable);
}

/// TC-PORT-APPR-5: an answerer that panics denies, and does not fail the turn.
///
/// Upstream: "contains a throwing answerer as unavailable" and "contains an
/// answerer that throws SYNCHRONOUSLY as unavailable".
///
/// The bus keeps `waterfall` loud by design, so this containment is the seam's
/// own and is deliberate: a question that cannot be answered has a defined
/// answer, and letting the panic unwind would fail the whole turn rather than
/// deny one call - a worse outcome, and a less safe one.
///
/// Input: an answerer that panics before it awaits anything, and one that
/// panics after.
/// Expected: `unavailable` both times, the pair still committed, and nothing
/// unwinds into the caller.
#[tokio::test]
async fn an_answerer_that_panics_denies_rather_than_failing_the_turn() {
    quiet_deliberate_panics();

    for after_await in [false, true] {
        let h = Fixture::in_turn().await;
        let _bug = h.bus.on_waterfall::<ApprovalAsk, _>(move |_ev, _next| {
            if !after_await {
                panic!("{DELIBERATE}");
            }
            Box::pin(async move {
                tokio::task::yield_now().await;
                panic!("{DELIBERATE}");
            })
        });

        let outcome = h
            .service()
            .request(ApprovalRequest::new("shell"), &Interrupt::default())
            .await
            .expect("a panicking answerer is not a failed ask");

        assert_eq!(outcome, ApprovalOutcome::Unavailable);
        assert_eq!(h.audit().len(), 2, "the pair is still complete");
        assert_eq!(h.audit()[1].data["outcome"], json!("unavailable"));
    }
}

/// TC-PORT-APPR-6: a word the engine does not know is not a grant.
///
/// Upstream: "normalizes a rogue non-vocabulary answer to unavailable".
///
/// Upstream normalizes at the boundary of a dynamically typed return; the
/// tetanus answerer returns a Rust enum, so the rogue value cannot arrive that
/// way. It arrives over the wire instead, which is why contract section 4.4.7
/// fixes the reading and `ApprovalOutcome::parse` implements it.
///
/// Input: each of the four words, then two that are not.
/// Expected: the four round trip; anything else reads as `unavailable`, and in
/// particular a word that merely looks permissive does not grant.
#[test]
fn an_unknown_word_reads_as_the_fail_closed_outcome() {
    for outcome in [
        ApprovalOutcome::AllowedOnce,
        ApprovalOutcome::Rejected,
        ApprovalOutcome::Cancelled,
        ApprovalOutcome::Unavailable,
    ] {
        assert_eq!(ApprovalOutcome::parse(outcome.as_str()), outcome);
    }

    for rogue in ["allowed-always", "yes"] {
        let read = ApprovalOutcome::parse(rogue);
        assert_eq!(read, ApprovalOutcome::Unavailable);
        assert!(!read.grants(), "`{rogue}` must not open a gate");
    }
}

/// TC-PORT-APPR-7: an interrupt withdraws the question.
///
/// Upstream: "settles cancelled immediately on an already-aborted signal
/// without asking anyone", "resolves cancelled when the signal aborts
/// mid-question and discards the late answer", and "resolves the answer when
/// the signal never aborts".
///
/// Input: three asks under one grant-happy answerer - one where the interrupt
/// already landed, one where it lands while the question is outstanding, and
/// one where it never lands.
/// Expected: `cancelled`, `cancelled`, then the grant. The first never
/// consults the answerer at all; the second discards the answer that was on its
/// way; the journal records `cancelled` in both, so a late answer cannot
/// reopen a decision the log already carries.
#[tokio::test]
async fn an_interrupt_withdraws_the_question_and_discards_a_late_answer() {
    // Already interrupted: nobody is asked.
    let h = Fixture::in_turn().await;
    let asked = Arc::new(AtomicUsize::new(0));
    let _counting = counts(&h.bus, Arc::clone(&asked), ApprovalOutcome::AllowedOnce);
    let interrupt = Interrupt::default();
    interrupt.stop();

    let outcome = h
        .service()
        .request(ApprovalRequest::new("shell"), &interrupt)
        .await
        .expect("outcome");

    assert_eq!(outcome, ApprovalOutcome::Cancelled);
    assert_eq!(asked.load(Ordering::SeqCst), 0, "no answerer was consulted");
    assert_eq!(h.audit()[1].data["outcome"], json!("cancelled"));

    // Interrupted mid-question: the answer that was coming is discarded.
    let h = Fixture::in_turn().await;
    let _slow = h.bus.on_waterfall::<ApprovalAsk, _>(|_ev, _next| {
        Box::pin(async move {
            // Long enough that the interrupt below wins the race on any
            // machine, without the case depending on how long.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            ApprovalOutcome::AllowedOnce
        })
    });
    let interrupt = Arc::new(Interrupt::default());
    let waker = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        waker.stop();
    });

    let outcome = h
        .service()
        .request(ApprovalRequest::new("shell"), &interrupt)
        .await
        .expect("outcome");

    assert_eq!(outcome, ApprovalOutcome::Cancelled);
    assert_eq!(h.audit()[1].data["outcome"], json!("cancelled"));

    // Never interrupted: the answerer's word stands.
    let h = Fixture::in_turn().await;
    let _answerer = grants(&h.bus, ApprovalOutcome::AllowedOnce);

    let outcome = h
        .service()
        .request(ApprovalRequest::new("shell"), &Interrupt::default())
        .await
        .expect("outcome");

    assert_eq!(outcome, ApprovalOutcome::AllowedOnce);
}

/// TC-PORT-APPR-8: `never` refuses without consulting anyone, and cannot be
/// answered around.
///
/// Upstream: "a never config rejects deterministically without consulting any
/// answerer", "the gate decides FIRST even against an answerer registered
/// before the service", and "never is unbypassable even by an answerer
/// PREPENDED after the service mounts".
///
/// The three upstream cases exist because a listener-shaped gate could be
/// ordered behind an eager granter. tetanus decides inside `request` for the
/// same reason, so the restatement is one case with the granter registered both
/// before and after the service: registration order must not matter at all.
///
/// Input: a `never` deployment with a granting answerer registered before the
/// service is built, and again with one registered after.
/// Expected: `rejected` both times, the answerer never consulted, and the pair
/// still on the journal - a refusal is a decision, and the log says one was
/// made.
#[tokio::test]
async fn never_refuses_without_asking_whatever_the_registration_order() {
    for granter_first in [true, false] {
        let h = Fixture::in_turn().await;
        let asked = Arc::new(AtomicUsize::new(0));

        let _early =
            granter_first.then(|| counts(&h.bus, Arc::clone(&asked), ApprovalOutcome::AllowedOnce));
        let service = h.service_with(ApprovalPolicy::Never);
        let _late = (!granter_first)
            .then(|| counts(&h.bus, Arc::clone(&asked), ApprovalOutcome::AllowedOnce));

        let outcome = service
            .request(ApprovalRequest::new("shell"), &Interrupt::default())
            .await
            .expect("outcome");

        assert_eq!(outcome, ApprovalOutcome::Rejected);
        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "a never policy consults nobody, whenever the answerer registered"
        );
        assert_eq!(
            h.audit().iter().map(|e| e.ty.as_str()).collect::<Vec<_>>(),
            [topic::APPROVAL_ASKED, topic::APPROVAL_DECIDED],
            "a refusal is a decision, and the journal records it"
        );
        assert_eq!(h.audit()[1].data["outcome"], json!("rejected"));
    }
}

/// TC-PORT-APPR-9: the session's own switch outranks the deployment default,
/// in both directions.
///
/// Upstream: "a session override outranks the configured default, in both
/// directions" and "folds to the last event, or undefined without one".
///
/// Input: a `never` deployment; read the override, switch to `ask`, ask, switch
/// back to `never`, ask.
/// Expected: no override at first; then `ask` and a grant; then `never` and a
/// refusal. The fold is the whole state, so this is also the resume story - a
/// journal replayed into a new process is under the policy its last switch
/// named.
#[tokio::test]
async fn a_session_switch_outranks_the_deployment_default_both_ways() {
    let h = Fixture::in_turn().await;
    let _answerer = grants(&h.bus, ApprovalOutcome::AllowedOnce);
    let service = h.service_with(ApprovalPolicy::Never);

    assert_eq!(service.override_of(), None, "nothing switched yet");
    assert_eq!(
        service.policy(),
        ApprovalPolicy::Never,
        "the default stands"
    );

    assert!(service.set_policy(ApprovalPolicy::Ask).expect("switch"));
    assert_eq!(service.override_of(), Some(ApprovalPolicy::Ask));
    assert_eq!(
        service
            .request(ApprovalRequest::new("shell"), &Interrupt::default())
            .await
            .expect("outcome"),
        ApprovalOutcome::AllowedOnce
    );

    assert!(service.set_policy(ApprovalPolicy::Never).expect("switch"));
    assert_eq!(
        service
            .request(ApprovalRequest::new("shell"), &Interrupt::default())
            .await
            .expect("outcome"),
        ApprovalOutcome::Rejected
    );

    // The fold reads the journal and nothing else, so a reader that only has
    // the log agrees with the service that wrote it.
    assert_eq!(
        effective_policy(&h.log.events()),
        Some(ApprovalPolicy::Never)
    );
}

/// TC-PORT-APPR-10: a policy outside the vocabulary is refused before the log
/// changes, and setting the current policy writes nothing.
///
/// Upstream: "rejects a policy outside the closed vocabulary before appending"
/// and "defaults a schema-less construction to ask".
///
/// Input: `ApprovalPolicy::parse` on both words and on one that is neither; a
/// switch to the policy the session is already under; a journal carrying a
/// policy word nothing can read.
/// Expected: the two parse and the third is refused; the idempotent switch
/// appends nothing and says so; an unreadable stored word folds to `None`, so
/// the deployment default takes over rather than a corrupt journal deciding
/// permissions.
#[tokio::test]
async fn a_policy_outside_the_vocabulary_is_refused_and_a_no_op_switch_is_silent() {
    assert_eq!(
        ApprovalPolicy::parse("ask").expect("ask"),
        ApprovalPolicy::Ask
    );
    assert_eq!(
        ApprovalPolicy::parse("never").expect("never"),
        ApprovalPolicy::Never
    );
    assert!(matches!(
        ApprovalPolicy::parse("sometimes"),
        Err(ApprovalError::UnknownPolicy(word)) if word == "sometimes"
    ));
    assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::Ask);

    let h = Fixture::in_turn().await;
    let service = h.service_with(ApprovalPolicy::Ask);
    let before = h.log.events().len();
    assert!(
        !service.set_policy(ApprovalPolicy::Ask).expect("no-op"),
        "switching to the current policy is not a switch"
    );
    assert_eq!(h.log.events().len(), before, "and appends nothing");

    // A word the fold cannot read is not a policy. It falls back to the
    // deployment default, which is the safe direction: a journal nobody can
    // parse must not be able to grant anything.
    h.log
        .append(topic::APPROVAL_POLICY, json!({ "policy": "sometimes" }))
        .expect("append");
    assert_eq!(effective_policy(&h.log.events()), None);
    assert_eq!(
        h.service_with(ApprovalPolicy::Never).policy(),
        ApprovalPolicy::Never
    );
}

/// TC-PORT-APPR-11: every ask gets its own id.
///
/// Upstream: "issues a fresh id per request".
///
/// Input: three asks on one session, then a second service over the same
/// journal - which is what a resume is.
/// Expected: four distinct ids. The resumed service matters: the pair is
/// matched by id, so an id minted twice would make two questions read as one
/// and leave a decision attached to the wrong ask.
#[tokio::test]
async fn every_ask_gets_an_id_of_its_own_across_a_resume() {
    let h = Fixture::in_turn().await;
    for _ in 0..3 {
        h.service()
            .request(ApprovalRequest::new("echo"), &Interrupt::default())
            .await
            .expect("outcome");
    }
    h.service()
        .request(ApprovalRequest::new("echo"), &Interrupt::default())
        .await
        .expect("outcome");

    let audit = h.audit();
    let ids: Vec<&str> = audit
        .iter()
        .filter(|e| e.ty == topic::APPROVAL_ASKED)
        .map(|e| e.data["id"].as_str().expect("id"))
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "ids collided: {ids:?}");
    assert_eq!(ids.len(), 4);
}

/// TC-PORT-APPR-12: the audit pair is one to one over any journal.
///
/// Upstream: `invariant.spec.ts` - "approval/asked repeated open id",
/// "approval/decided has no matching approval/asked", and both "appended
/// outside any open turn" rules.
///
/// Upstream enforces these in a validator every append passes through; tetanus
/// has none, so the claim is about the writer instead: whatever the asks did,
/// the journal the service left is well formed.
///
/// Input: a run mixing a grant, a refusal under `never`, a contained panic and
/// a denial with nobody listening.
/// Expected: every ask has exactly one decision with its id; every decision
/// answers an ask that came before it; no id is asked twice; every audit event
/// is inside the open turn; and every outcome is one of the four words.
#[tokio::test]
async fn every_journal_the_service_writes_is_a_well_formed_audit() {
    quiet_deliberate_panics();
    let h = Fixture::in_turn().await;

    let granting = grants(&h.bus, ApprovalOutcome::AllowedOnce);
    h.service()
        .request(ApprovalRequest::new("a"), &Interrupt::default())
        .await
        .expect("outcome");
    drop(granting);

    h.service_with(ApprovalPolicy::Never)
        .request(ApprovalRequest::new("b"), &Interrupt::default())
        .await
        .expect("outcome");

    let bug = h
        .bus
        .on_waterfall::<ApprovalAsk, _>(|_ev, _next| panic!("{DELIBERATE}"));
    h.service()
        .request(ApprovalRequest::new("c"), &Interrupt::default())
        .await
        .expect("outcome");
    drop(bug);

    h.service()
        .request(ApprovalRequest::new("d"), &Interrupt::default())
        .await
        .expect("outcome");

    let events = h.log.events();
    let mut open: Vec<String> = Vec::new();
    let mut pairs = 0;
    for event in &events {
        assert!(
            has_open_turn(&events[..=event.seq as usize]),
            "every audit event is inside the open turn"
        );
        match event.ty.as_str() {
            topic::APPROVAL_ASKED => {
                let id = event.data["id"].as_str().expect("id").to_string();
                assert!(!open.contains(&id), "id {id} was asked while still open");
                open.push(id);
            }
            topic::APPROVAL_DECIDED => {
                let id = event.data["id"].as_str().expect("id").to_string();
                let held = open.iter().position(|held| *held == id);
                assert!(held.is_some(), "decision for {id} answers no ask");
                open.remove(held.expect("checked"));
                let word = event.data["outcome"].as_str().expect("outcome");
                assert_eq!(
                    ApprovalOutcome::parse(word).as_str(),
                    word,
                    "{word} is not one of the four"
                );
                pairs += 1;
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "questions left unanswered: {open:?}");
    assert_eq!(pairs, 4, "one decision per ask");
}

// ---------------------------------------------------------------- fixtures

const DELIBERATE: &str = "deliberate: an approval answerer with a bug";

struct Fixture {
    bus: EventBus,
    log: Arc<dyn SessionLog>,
    _dir: TempDir,
}

impl Fixture {
    /// A journal with a header and nothing else, for the cases about what an
    /// ask does when no turn is open.
    async fn bare() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create("approval", dir.path().join("a.jsonl"), bus.clone())
                .expect("journal");
        Self {
            bus,
            log,
            _dir: dir,
        }
    }

    /// The same, with a turn open, which is the precondition every ask has.
    async fn in_turn() -> Self {
        let h = Self::bare().await;
        h.log
            .append(topic::TURN_START, json!({ "turn": 1 }))
            .expect("append");
        h
    }

    fn service(&self) -> Arc<ApprovalService> {
        self.service_with(ApprovalPolicy::Ask)
    }

    fn service_with(&self, policy: ApprovalPolicy) -> Arc<ApprovalService> {
        ApprovalService::new(self.bus.clone(), Arc::clone(&self.log), policy)
    }

    /// Only the audit events, so a case asserts the pair without counting the
    /// turn boundary it was written inside.
    fn audit(&self) -> Vec<SessionEvent> {
        self.log
            .events()
            .into_iter()
            .filter(|e| e.ty == topic::APPROVAL_ASKED || e.ty == topic::APPROVAL_DECIDED)
            .collect()
    }
}

/// An answerer that always returns the same word.
fn grants(bus: &EventBus, outcome: ApprovalOutcome) -> EffectHandle {
    bus.on_waterfall::<ApprovalAsk, _>(move |_ev, _next| Box::pin(async move { outcome }))
}

/// The same, counting how many times it was consulted - which is the whole
/// assertion for the cases about a gate that must not dispatch.
fn counts(bus: &EventBus, seen: Arc<AtomicUsize>, outcome: ApprovalOutcome) -> EffectHandle {
    bus.on_waterfall::<ApprovalAsk, _>(move |_ev, _next| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { outcome })
    })
}

static QUIET: Once = Once::new();

/// Drop the panic report for exactly the payload this suite panics with, and
/// pass every other panic - a failed assertion, a real bug - straight through.
fn quiet_deliberate_panics() {
    QUIET.call_once(|| {
        let inherited = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = info
                .payload()
                .downcast_ref::<&str>()
                .is_some_and(|message| *message == DELIBERATE);
            if !ours {
                inherited(info);
            }
        }));
    });
}
