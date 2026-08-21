//! Test Design Specification: user questions, ported.
//!
//! Feature under test: `tetanus_turn::questions` - what a tool may ask a
//! person, what counts as an answer, what the journal records, and what happens
//! when nobody answers. Upstream pins the same surface in
//! `packages/interaction/user-questions/tests/user-questions.spec.ts` and
//! `packages/interaction/tool-ask-user/tests/tool-ask-user.spec.ts`; contract
//! section 4.4.3 settled the rules before either was built here, and each case
//! names the rule it restates.
//!
//! Approach: a real journal in a temporary directory with a `turn/start`
//! written to satisfy the enclosure precondition, and an `EventBus` for
//! answerers - the same shape `upstream_approval.rs` uses, because the two
//! seams make the same promises and a reader comparing them should not have to
//! compare two fixtures as well.
//!
//! What is not restated, and why. Upstream's `plan-review` presentation intent
//! changes how a surface draws a question and nothing about the protocol, so it
//! belongs to the presentation lane by `docs/interface-contract.md` §5. Its
//! `ui/ask` wire frame is reserved rather than served, so there is no wire half
//! to assert. Its scoped routing has no counterpart, for the reason
//! `upstream_approval.rs` gives: one bus per session.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime. One
//! case panics on purpose and drops exactly its payload from the hook.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic that escapes the seam.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use serde_json::json;
use tempfile::TempDir;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::log::topic;
use tetanus_turn::questions::{
    Answer, AskUser, AskUserTool, Question, QuestionError, QuestionOption, QuestionService,
};
use tetanus_turn::tools::{Tool, ToolRegistry};

struct Fixture {
    log: Arc<dyn SessionLog>,
    bus: EventBus,
    interrupt: Arc<Interrupt>,
    _dir: TempDir,
}

impl Fixture {
    /// A journal with a turn open, which is what asking requires.
    fn open() -> Self {
        let fixture = Self::bare();
        fixture
            .log
            .append(topic::TURN_START, json!({ "turn": 1 }))
            .expect("turn/start");
        fixture
    }

    fn bare() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create("questions", dir.path().join("q.jsonl"), bus.clone())
                .expect("journal");
        Self {
            log,
            bus,
            interrupt: Interrupt::new(),
            _dir: dir,
        }
    }

    fn service(&self) -> Arc<QuestionService> {
        QuestionService::new(
            self.bus.clone(),
            Arc::clone(&self.log),
            Arc::clone(&self.interrupt),
        )
    }

    fn events(&self, ty: &str) -> Vec<SessionEvent> {
        self.log
            .events()
            .into_iter()
            .filter(|event| event.ty == ty)
            .collect()
    }

    /// An answerer that returns what the case tells it to, counting the asks.
    fn answering(&self, answers: Option<Vec<Answer>>) -> (EffectHandle, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let handle = self.bus.on_waterfall::<AskUser, _>(move |_ev, _next| {
            counter.fetch_add(1, Ordering::SeqCst);
            let answers = answers.clone();
            Box::pin(async move { answers })
        });
        (handle, asked)
    }
}

fn choice(id: &str) -> Question {
    Question::new(id, "Which approach?").offering(["rewrite", "patch"])
}

/// TC-PORT-INT-17: an answered question is recorded as one pair, and the answer
/// comes back.
///
/// Upstream: the tool "pauses until a UI provider returns a human answer, then
/// feeds that answer back into the agent loop".
///
/// Input: one question with options, answered with one of them.
/// Expected: the answer is returned; `question/asked` and `question/answered`
/// share one id; the answered half says so and carries what was said. The pair
/// is what makes a transcript able to explain why the tool did what it did
/// next.
#[tokio::test]
async fn an_answered_question_returns_the_answer_and_records_one_pair() {
    let h = Fixture::open();
    let (_answerer, asked) = h.answering(Some(vec![Answer::choosing("approach", ["patch"])]));

    let answers = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect("asked");

    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(answers, Some(vec![Answer::choosing("approach", ["patch"])]));
    let opened = h.events(topic::QUESTION_ASKED);
    let closed = h.events(topic::QUESTION_ANSWERED);
    assert_eq!(opened.len(), 1);
    assert_eq!(closed.len(), 1);
    assert_eq!(opened[0].data["id"], closed[0].data["id"]);
    assert_eq!(closed[0].data["answered"], true);
    assert_eq!(closed[0].data["answers"][0]["selected"][0], "patch");
    assert_eq!(
        opened[0].data["questions"][0]["question"],
        "Which approach?"
    );
}

/// TC-PORT-INT-18: an answer that misses a question is no answer at all.
///
/// Upstream, and contract §4.4.3: "A tool that asked three things needs three;
/// given two it is in a state its author never wrote code for."
///
/// Input: two questions, answered with one.
/// Expected: `None`, and the pair recorded as unanswered. The tool then meets
/// one of exactly two cases rather than a partial third it never wrote code
/// for.
#[tokio::test]
async fn a_partial_answer_is_treated_as_no_answer() {
    let h = Fixture::open();
    let (_answerer, _asked) = h.answering(Some(vec![Answer::choosing("first", ["rewrite"])]));

    let answers = h
        .service()
        .ask(vec![choice("first"), choice("second")])
        .await
        .expect("asked");

    assert_eq!(answers, None);
    assert_eq!(
        h.events(topic::QUESTION_ANSWERED)[0].data["answered"],
        false
    );
}

/// TC-PORT-INT-19: an answer naming a question nobody asked is ignored.
///
/// Contract §4.4.3: "the questions are the contract, and a client that answered
/// more has not answered less".
///
/// Input: one question, answered with that one plus an extra.
/// Expected: the answer stands, and only the asked question's answer is
/// carried. Refusing the whole answer would punish a client for being generous.
#[tokio::test]
async fn an_answer_to_a_question_nobody_asked_is_dropped_not_refused() {
    let h = Fixture::open();
    let (_answerer, _asked) = h.answering(Some(vec![
        Answer::choosing("approach", ["patch"]),
        Answer::choosing("invented", ["whatever"]),
    ]));

    let answers = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect("asked");

    let answers = answers.expect("the asked question was answered");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].id, "approach");
}

/// TC-PORT-INT-20: an answer outside the offered labels is not an answer, and
/// a single-select given several is not either.
///
/// Contract §4.4.3: "`QuestionOption.label` is both the text and the value, so
/// a question that offers options accepts those labels and nothing else", and
/// "a single-select question given several labels is unanswered, not
/// first-wins".
///
/// Input: a label that was never offered; then two labels for a single-select
/// question.
/// Expected: `None` both times. First-wins would have a tool act on a guess
/// about which one the user meant, which is worse than telling it there is no
/// answer.
#[tokio::test]
async fn an_answer_outside_the_options_and_a_double_answer_are_both_refused() {
    for given in [
        Answer::choosing("approach", ["something else"]),
        Answer::choosing("approach", ["rewrite", "patch"]),
    ] {
        let h = Fixture::open();
        let (_answerer, _asked) = h.answering(Some(vec![given]));

        let answers = h
            .service()
            .ask(vec![choice("approach")])
            .await
            .expect("asked");

        assert_eq!(answers, None);
    }
}

/// TC-PORT-INT-21: a multi-select question takes several, and a free-text one
/// takes anything.
///
/// Contract §4.4.3: "A question with no options is free text and accepts
/// anything."
///
/// Input: a multi-select question answered with two of its labels, and a
/// free-text question answered with a sentence.
/// Expected: both stand. The rule that closes the list is the *options*, so a
/// question that offered none closes nothing.
#[tokio::test]
async fn a_multi_select_takes_several_and_free_text_takes_anything() {
    let h = Fixture::open();
    let (_answerer, _asked) = h.answering(Some(vec![
        Answer::choosing("files", ["a.rs", "b.rs"]),
        Answer::saying("name", "call it the parser"),
    ]));

    let answers = h
        .service()
        .ask(vec![
            Question::new("files", "Which files?")
                .offering([
                    QuestionOption::new("a.rs"),
                    QuestionOption::new("b.rs").describing("the newer one"),
                ])
                .multi(),
            Question::new("name", "What should it be called?"),
        ])
        .await
        .expect("asked");

    let answers = answers.expect("both were answered");
    assert_eq!(answers[0].selected, ["a.rs", "b.rs"]);
    assert_eq!(answers[1].custom.as_deref(), Some("call it the parser"));
}

/// TC-PORT-INT-22: with nobody listening, the ask settles unanswered at once.
///
/// Upstream: the seam has a terminal, and a headless deployment reaches it.
///
/// Input: an ask on a bus with no answerer.
/// Expected: `None`, promptly, with the pair on the journal. There is no
/// timeout by design (§4.4.3), so an ask that waited for a listener that does
/// not exist would hang a headless run for ever - this is the case that says
/// it does not.
#[tokio::test]
async fn with_nobody_listening_the_ask_settles_unanswered() {
    let h = Fixture::open();

    let answers = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect("asked");

    assert_eq!(answers, None);
    assert_eq!(h.events(topic::QUESTION_ANSWERED).len(), 1);
    assert_eq!(
        h.events(topic::QUESTION_ANSWERED)[0].data["answered"],
        false
    );
}

/// TC-PORT-INT-23: an interrupt withdraws an outstanding question.
///
/// Contract §4.4.3: "`agent.interrupt` settles an outstanding ask as unanswered
/// at once, rather than waiting for an answer a stopped turn would not use, and
/// a late answer is discarded."
///
/// Input: an answerer that takes far longer than the case will wait, with an
/// interrupt raised while the question is outstanding.
/// Expected: the ask settles unanswered without waiting for the answerer. The
/// interrupt is the only way out of an unbounded wait, so a seam that ignored
/// it would make a slow answerer indistinguishable from a wedged turn.
#[tokio::test]
async fn an_interrupt_withdraws_an_outstanding_question() {
    let h = Fixture::open();
    let _slow = h.bus.on_waterfall::<AskUser, _>(|_ev, _next| {
        Box::pin(async move {
            // Long enough that the interrupt below wins on any machine, without
            // the case depending on how long.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Some(vec![Answer::choosing("approach", ["patch"])])
        })
    });
    let waker = Arc::clone(&h.interrupt);
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        waker.stop();
    });

    let answers = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect("asked");

    assert_eq!(answers, None);
    assert_eq!(
        h.events(topic::QUESTION_ANSWERED)[0].data["answered"],
        false
    );
}

/// TC-PORT-INT-24: a question nothing could answer is refused before anything
/// is written.
///
/// Upstream: `ask()` rejects a malformed request rather than showing it.
///
/// Input: no questions at all; two questions sharing an id; an option offered
/// twice; and a multi-select with nothing to select.
/// Expected: each is refused as malformed and appends nothing. A surface must
/// never be shown a prompt for which no valid answer exists, and a journal must
/// never carry an ask that could only ever be closed unanswered.
#[tokio::test]
async fn a_question_nothing_could_answer_is_refused_and_writes_nothing() {
    let cases = vec![
        vec![],
        vec![choice("same"), choice("same")],
        vec![Question::new("q", "Which?").offering(["one", "one"])],
        vec![Question::new("q", "Which?").multi()],
        vec![Question::new("", "Which?")],
    ];

    for questions in cases {
        let h = Fixture::open();
        let before = h.log.events().len();

        let refused = h.service().ask(questions).await.expect_err("refused");

        assert!(matches!(refused, QuestionError::Malformed(_)), "{refused}");
        assert_eq!(h.log.events().len(), before, "nothing was appended");
    }
}

/// TC-PORT-INT-25: asking outside an open turn is refused, and writes nothing.
///
/// Contract §4.4.3, pointing at §4.4.7: the pair is turn-enclosed because the
/// turn is what crash repair closes.
///
/// Input: an ask on a journal that never opened a turn.
/// Expected: refused, with nothing appended - a refused ask must not leave half
/// a pair behind, and a question outside a turn could never be closed.
#[tokio::test]
async fn asking_outside_an_open_turn_is_refused() {
    let h = Fixture::bare();

    let refused = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect_err("refused");

    assert!(matches!(refused, QuestionError::NoOpenTurn), "{refused}");
    assert!(h.log.events().is_empty());
}

/// TC-PORT-INT-26: an answerer that panics leaves the tool without an answer,
/// not the turn without a step.
///
/// Upstream contains a throwing provider the same way.
///
/// Input: an answerer that panics.
/// Expected: `None`, the pair recorded, and no panic escaping. The bus keeps
/// `waterfall` loud on purpose, and this is the same deliberate exception the
/// approval seam takes: a question that cannot be answered has a defined
/// outcome, and unwinding would fail the turn instead.
#[tokio::test]
async fn an_answerer_that_panics_leaves_the_question_unanswered() {
    quieten_deliberate_panics();
    let h = Fixture::open();
    let _bug = h.bus.on_waterfall::<AskUser, _>(|_ev, _next| {
        Box::pin(async move {
            tokio::task::yield_now().await;
            panic!("{DELIBERATE}");
        })
    });

    let answers = h
        .service()
        .ask(vec![choice("approach")])
        .await
        .expect("asked");

    assert_eq!(answers, None);
    assert_eq!(h.events(topic::QUESTION_ANSWERED).len(), 1);
}

/// TC-PORT-INT-27: the tool hands the answer to the model, and says plainly
/// when there is none.
///
/// Upstream: the answer is fed back "as an ordinary tool result".
///
/// Input: the tool called through a registry, once with an answerer and once
/// without.
/// Expected: an answered call is `ok` and carries the answers as JSON the model
/// can parse; an unanswered one is a failed result whose text says what to do
/// instead. A tool that failed the turn because nobody was at the keyboard
/// would make an unattended run impossible.
#[tokio::test]
async fn the_tool_returns_the_answer_or_says_there_is_none() {
    let arguments = json!({
        "questions": [{
            "id": "approach",
            "question": "Which approach?",
            "options": [{ "label": "rewrite" }, { "label": "patch" }],
        }],
    });

    let answered = Fixture::open();
    let (_answerer, _asked) =
        answered.answering(Some(vec![Answer::choosing("approach", ["patch"])]));
    let with_answer = AskUserTool::new(answered.service())
        .execute(&arguments)
        .await
        .expect("the tool answered");

    let silent = Fixture::open();
    let without = AskUserTool::new(silent.service())
        .execute(&arguments)
        .await
        .expect("the tool answered");

    assert!(with_answer.ok);
    let parsed: serde_json::Value =
        serde_json::from_str(&with_answer.content).expect("the model is handed parseable JSON");
    assert_eq!(parsed["answers"][0]["id"], "approach");
    assert_eq!(parsed["answers"][0]["selected"][0], "patch");
    assert!(!without.ok);
    assert!(
        without.content.contains("did not answer"),
        "{}",
        without.content
    );
    assert!(
        without.content.contains("Continue without it"),
        "the model is told what to do instead: {}",
        without.content
    );
}

/// TC-PORT-INT-28: a question a crash caught mid-flight is closed on reopen.
///
/// Contract §4.4.4 and §4.4.3: "an ask a crash caught mid-question is closed on
/// reopen the way an approval is".
///
/// Input: a journal holding a `question/asked` with no answer, inside a turn
/// that never ended.
/// Expected: repair appends `question/answered` with `answered: false` for it,
/// before the turn's own closers; an already-answered ask is untouched. Without
/// it the journal would carry a question with no answer for the rest of its
/// life.
#[tokio::test]
async fn a_question_the_crash_caught_is_closed_unanswered_on_reopen() {
    let h = Fixture::open();
    h.log
        .append(
            topic::QUESTION_ASKED,
            json!({ "id": "ask-1", "questions": [] }),
        )
        .expect("first ask");
    h.log
        .append(
            topic::QUESTION_ANSWERED,
            json!({ "id": "ask-1", "answers": [], "answered": true }),
        )
        .expect("first answer");
    h.log
        .append(
            topic::QUESTION_ASKED,
            json!({ "id": "ask-2", "questions": [] }),
        )
        .expect("second ask");

    let closers = tetanus_turn::repair::repair(h.log.as_ref()).expect("repair");

    let types: Vec<&str> = closers.iter().map(|event| event.ty.as_str()).collect();
    assert_eq!(types, [topic::QUESTION_ANSWERED, topic::TURN_END]);
    assert_eq!(closers[0].data["id"], "ask-2");
    assert_eq!(closers[0].data["answered"], false);
}

/// The payload the deliberate panic carries, so the hook drops exactly it.
const DELIBERATE: &str = "deliberate: a question answerer with a bug";

/// Keep the deliberate panic out of the test output without hiding any other.
fn quieten_deliberate_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info.payload();
            let deliberate = payload
                .downcast_ref::<&str>()
                .is_some_and(|text| text.contains(DELIBERATE));
            if !deliberate {
                previous(info);
            }
        }));
    });
}

/// A registry the ask tool can be dispatched through, so the case above is
/// about the tool and not about a direct call nothing else makes.
#[tokio::test]
async fn the_tool_registers_under_its_documented_name() {
    let h = Fixture::open();
    let registry = ToolRegistry::new().with(AskUserTool::new(h.service()));

    let schemas = registry.schemas();

    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, AskUserTool::NAME);
    assert_eq!(schemas[0].parameters["required"], json!(["questions"]));
}
