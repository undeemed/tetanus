//! Test Design Specification: the tool pipeline's scheduling invariants, as
//! properties.
//!
//! Feature under test: what one step's tool calls guarantee for *every*
//! schedule, not for the five upstream chose. `TurnEngine::run_tool_calls` and
//! `run_tool_group` decide which calls may overlap, how many run at once, and
//! the order results are committed in; `upstream_tool_calls.rs` pins five
//! example schedules (TC-PORT-TOOL-1..5) and this file states the rules those
//! examples are instances of. Upstream keeps a `properties.spec.ts` beside
//! `core/tools`, but it is about a schema DSL tetanus does not have - a tool
//! declares its JSON Schema directly - so the properties here are the pipeline
//! ones its gap list names rather than a translation of that file.
//!
//! Approach: generate a schedule - one to six calls, each parallel-safe or
//! exclusive, each with its own yield count - and a pool cap of one to four,
//! then run it through a real turn against the offline fixture. The tools
//! record an ordered start/end trace, so overlap is reconstructed exactly
//! rather than sampled: replaying the trace with a live set gives the set of
//! calls in flight at every instant the schedule reached. Ordering comes from
//! yield counts on a single-threaded runtime, so no case sleeps and no case
//! depends on a clock.
//!
//! Why generation finds what examples cannot: a cap, a run of parallel calls
//! and a barrier interact. TC-PORT-TOOL-2 has a barrier under the default cap
//! and TC-PORT-TOOL-4 has a cap with no barrier, so no example covers a
//! barrier arriving while a capped pool is still draining - which is the
//! `started > 0` break in `run_tool_group` and the one place the two rules
//! meet.
//!
//! Features NOT tested here: what a tool's body does (`upstream_tools.rs`),
//! how a call is classified (`upstream_execution_mode.rs`), and what the whole
//! turn writes (`upstream_session_invariants.rs`). Those are the neighbouring
//! rules; this file only pins the schedule.
//!
//! Environmental needs: a writable temp directory and a Tokio runtime. No case
//! reaches a network or an API key. One case here panics on purpose, so the
//! suite installs a hook that drops exactly its own payload and passes every
//! other panic through.
//!
//! Pass criteria: each case's stated expected result holds for every generated
//! schedule.
//! Fail criteria: any counterexample, or a panic that escapes the pipeline.

// This suite drives the fixture's engine with its own provider and tools; a
// test binary lints the parts of a shared fixture it does not reach for.
#[allow(dead_code)]
mod harness;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, Once};

use harness::Harness;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::json;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::ModelResponse;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{
    Tool, ToolCall, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};
use tetanus_turn::TurnConfig;

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// TC-PROP-TOOL-1: every call the model asked for is answered exactly
    /// once, and the answers are on the journal in the order it asked.
    ///
    /// This is the rule TC-PORT-TOOL-1, -3 and -4 are three examples of, and
    /// the one a resumed transcript depends on: a history whose results are in
    /// completion order is a different conversation on every replay, because
    /// completion order is not a fact the journal records.
    ///
    /// Input: any schedule of one to six calls under any cap of one to four.
    /// Expected: the committed `call_id`s are exactly the asked-for ids, in
    /// the asked-for order, with no id missing, added or repeated.
    #[test]
    fn every_call_is_answered_once_and_in_model_order(plan in schedule()) {
        run(&plan, |journal, _trace| {
            prop_assert_eq!(
                committed(journal),
                plan.ids(),
                "the journal reads in model order, whatever settled first"
            );
            Ok(())
        })?;
    }

    /// TC-PROP-TOOL-2: an exclusive call is alone for the whole time it runs.
    ///
    /// A barrier that overlapped anything would be no barrier at all, and the
    /// tools that ask for one ask because sharing the step is what they cannot
    /// survive. The claim is stated over every instant rather than over the
    /// peak count, because a peak of two proves an overlap happened and a peak
    /// of one does not prove the barrier was the reason.
    ///
    /// Input: any schedule, so a barrier may open the step, close it, sit
    /// between two parallel runs, or arrive while a capped pool is draining.
    /// Expected: at every instant an exclusive call is in flight, it is the
    /// only call in flight.
    #[test]
    fn an_exclusive_call_never_shares_the_step(plan in schedule()) {
        run(&plan, |_journal, trace| {
            for live in trace.instants() {
                let barriers: Vec<&String> = live.iter().filter(|id| plan.is_exclusive(id)).collect();
                prop_assert!(
                    barriers.is_empty() || live.len() == 1,
                    "{barriers:?} ran beside {live:?}"
                );
            }
            Ok(())
        })?;
    }

    /// TC-PROP-TOOL-3: the pool never runs more calls at once than the cap
    /// allows, and still runs all of them.
    ///
    /// The cap is a limit on overlap, not on how many calls a step may make -
    /// a pool that honoured it by dropping work would pass a peak assertion
    /// and lose a tool result. Both halves are asserted together for that
    /// reason.
    ///
    /// Input: any schedule under any cap of one to four.
    /// Expected: no instant has more calls in flight than the cap, and every
    /// call still started and ended.
    #[test]
    fn the_pool_holds_the_cap_without_dropping_work(plan in schedule()) {
        run(&plan, |_journal, trace| {
            for live in trace.instants() {
                prop_assert!(
                    live.len() <= plan.cap,
                    "{} in flight under a cap of {}: {live:?}",
                    live.len(),
                    plan.cap
                );
            }
            let mut ended = trace.ends();
            ended.sort();
            let mut asked = plan.ids();
            asked.sort();
            prop_assert_eq!(ended, asked, "the pool replenished until the step was done");
            Ok(())
        })?;
    }

    /// TC-PROP-TOOL-4: a result names the call it answers, and cites it,
    /// whatever order the calls reached the journal in.
    ///
    /// This is the one place overlap is visible on the log. A `tool/call` is
    /// appended when its call is *dispatched* and a `tool/result` when it is
    /// *committed*, and those are two different orders: under a cap of two a
    /// later call can be logged before an earlier one is answered. So the
    /// pairing cannot be positional, and contract section 4.3 promises it is
    /// not - a surface pairs by `call_id`, and `sourceEventSeqs` carries the
    /// same pairing for a reader that started mid-turn.
    ///
    /// Input: any schedule. Ids are unique but tool *names* repeat, so a
    /// pairing that matched on name would have several candidates.
    /// Expected: every result cites exactly one seq, that seq holds a
    /// `tool/call`, and that call's `id` is the result's `call_id`.
    #[test]
    fn a_result_cites_the_call_it_answers(plan in schedule()) {
        run(&plan, |journal, _trace| {
            let calls: Vec<&SessionEvent> =
                journal.iter().filter(|event| event.ty == topic::TOOL_CALL).collect();
            prop_assert_eq!(calls.len(), plan.calls.len(), "one tool/call per asked call");

            for result in journal.iter().filter(|event| event.ty == topic::TOOL_RESULT) {
                let cited = result.source_event_seqs.clone().unwrap_or_default();
                prop_assert_eq!(cited.len(), 1, "a result answers one call: {:?}", cited);

                let call = journal
                    .iter()
                    .find(|event| event.seq == cited[0])
                    .expect("a cited seq is a line of this journal");
                prop_assert_eq!(&call.ty, topic::TOOL_CALL, "a result cites a call");
                prop_assert_eq!(
                    call.data["id"].as_str(),
                    result.data["call_id"].as_str(),
                    "the cited call is the one this result answers"
                );
                prop_assert_eq!(
                    call.data["name"].as_str(),
                    result.data["name"].as_str(),
                    "and it is the same tool"
                );
            }
            Ok(())
        })?;
    }

    /// TC-PROP-TOOL-5: one call's panic costs that call its result and costs
    /// its siblings nothing.
    ///
    /// `upstream_tools.rs` pins containment for a step of one call. The rule
    /// that matters to a scheduler is the harder one: a body that unwinds
    /// inside a pool must not take the pool's other futures with it, must not
    /// leave a gap in the committed order, and must not end the turn.
    ///
    /// Input: any schedule, with exactly one of its calls replaced by a call
    /// on a tool whose body panics.
    /// Expected: the turn succeeds; every call still has exactly one result in
    /// model order; the panicking call's result is `ok: false`; and every
    /// other call's result is `ok: true`.
    #[test]
    fn a_panic_inside_a_pool_costs_only_its_own_call(plan in schedule_with_a_panic()) {
        quiet_deliberate_panics();
        run(&plan, |journal, _trace| {
            prop_assert_eq!(committed(journal), plan.ids(), "the step is still complete");

            for result in journal.iter().filter(|event| event.ty == topic::TOOL_RESULT) {
                let id = result.data["call_id"].as_str().unwrap_or_default();
                let ok = result.data["ok"].as_bool().unwrap_or_default();
                prop_assert_eq!(
                    ok,
                    !plan.is_panicking(id),
                    "`{}` reported ok: {}",
                    id,
                    ok
                );
            }
            Ok(())
        })?;
    }

    /// TC-PROP-TOOL-6: the journal a schedule writes is a function of that
    /// schedule.
    ///
    /// Overlap makes completion order a race; the durable record must not
    /// inherit it. Running the same schedule twice and comparing the journals
    /// is the direct statement of that, and it is what makes the other five
    /// properties worth asserting once: a rule that held only on the run that
    /// observed it would say nothing about the next one.
    ///
    /// Input: any schedule, run twice against two fresh journals.
    /// Expected: both runs commit the same results, in the same order, with
    /// the same outcomes - even where the two runs finished their tools in
    /// different orders.
    #[test]
    fn the_same_schedule_writes_the_same_journal(plan in schedule()) {
        let first = journal_of(&plan);
        let second = journal_of(&plan);
        prop_assert_eq!(first, second, "the record is the schedule, not the race");
    }
}

// ---------------------------------------------------------------- the plan

/// The parallel-safe tool. Its calls may overlap.
const SAFE: &str = "safe";
/// The exclusive tool. Its calls are barriers.
const SOLE: &str = "sole";
/// The tool that never returns a value.
const BOOM: &str = "boom";
/// What the panicking tool panics with, so the hook can drop exactly this.
const DELIBERATE: &str = "deliberate: a generated schedule's panicking call";

/// One generated call: which tool, and how long it stays in flight.
#[derive(Debug, Clone)]
struct Planned {
    tool: &'static str,
    yields: u64,
}

/// One generated schedule: the calls of a single step, and the pool they run
/// under.
#[derive(Debug, Clone)]
struct Plan {
    calls: Vec<Planned>,
    cap: usize,
}

impl Plan {
    /// The call ids, in the order the model asks for them. Ids are positional
    /// so a counterexample reads as a schedule rather than as a list of names.
    fn ids(&self) -> Vec<String> {
        (0..self.calls.len()).map(id).collect()
    }

    fn tool_of(&self, id: &str) -> Option<&'static str> {
        self.ids()
            .iter()
            .position(|mine| mine == id)
            .map(|index| self.calls[index].tool)
    }

    fn is_exclusive(&self, id: &str) -> bool {
        self.tool_of(id) == Some(SOLE)
    }

    fn is_panicking(&self, id: &str) -> bool {
        self.tool_of(id) == Some(BOOM)
    }

    fn asked(&self) -> Vec<ToolCall> {
        self.calls
            .iter()
            .enumerate()
            .map(|(index, planned)| ToolCall {
                id: id(index),
                name: planned.tool.to_string(),
                arguments: json!({ "id": id(index), "yields": planned.yields }),
            })
            .collect()
    }
}

fn id(index: usize) -> String {
    format!("call-{index}")
}

/// Any schedule of one to six calls under a cap of one to four. A yield count
/// of zero to four is enough to order any six calls against each other, and
/// keeps a generated case to a handful of scheduler turns.
fn schedule() -> impl Strategy<Value = Plan> {
    (
        prop::collection::vec(
            (prop::sample::select(vec![SAFE, SOLE]), 0..5u64)
                .prop_map(|(tool, yields)| Planned { tool, yields }),
            1..7,
        ),
        1..5usize,
    )
        .prop_map(|(calls, cap)| Plan { calls, cap })
}

/// The same, with exactly one call replaced by one that panics. One and not
/// "any number" on purpose: the claim is that a panic is contained to its own
/// call, and a schedule where everything panics cannot show a survivor.
fn schedule_with_a_panic() -> impl Strategy<Value = Plan> {
    schedule().prop_flat_map(|plan| {
        let len = plan.calls.len();
        (Just(plan), 0..len).prop_map(|(mut plan, victim)| {
            plan.calls[victim].tool = BOOM;
            plan
        })
    })
}

// ------------------------------------------------------------- the fixture

/// Run one schedule and hand the journal and the overlap trace to a claim.
fn run<F>(plan: &Plan, claim: F) -> Result<(), TestCaseError>
where
    F: FnOnce(&[SessionEvent], &Trace) -> Result<(), TestCaseError>,
{
    let (journal, trace) = drive(plan);
    claim(&journal, &trace)
}

/// The committed results of one run, as the shape TC-PROP-TOOL-6 compares.
fn journal_of(plan: &Plan) -> Vec<(String, bool)> {
    let (journal, _trace) = drive(plan);
    journal
        .iter()
        .filter(|event| event.ty == topic::TOOL_RESULT)
        .map(|event| {
            (
                event.data["call_id"].as_str().unwrap_or_default().into(),
                event.data["ok"].as_bool().unwrap_or_default(),
            )
        })
        .collect()
}

/// Take one schedule through a real turn on its own journal.
///
/// A current-thread runtime is deliberate: the scheduler's overlap must come
/// from its own pool rather than from spare threads, so a cap that is not
/// enforced shows up as an overlap here instead of being hidden by a runtime
/// that was serialising anyway.
fn drive(plan: &Plan) -> (Vec<SessionEvent>, Trace) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let trace = Trace::default();
        let h = Harness::with_config(
            "properties-tools",
            trace.registry(),
            TurnConfig {
                max_parallel_tool_calls: plan.cap.try_into().expect("a cap of at least one"),
                ..TurnConfig::default()
            },
        )
        .await;
        let _provider = asks_for(h.bus(), plan.asked());

        h.engine
            .run_turn("a generated schedule")
            .await
            .expect("a schedule of contained tools is not a turn failure");

        (replay(&h.log_path), trace)
    })
}

/// One start or end, in the order the pipeline reached it.
#[derive(Debug, Clone)]
enum Note {
    Start(String),
    End(String),
}

/// The ordered record of what was in flight, shared by every generated tool.
#[derive(Clone, Default)]
struct Trace {
    notes: Arc<Mutex<Vec<Note>>>,
}

impl Trace {
    fn registry(&self) -> ToolRegistry {
        ToolRegistry::new()
            .with(Arc::new(Probe {
                name: SAFE,
                mode: ToolMode::Parallel,
                panics: false,
                trace: self.clone(),
            }))
            .with(Arc::new(Probe {
                name: SOLE,
                mode: ToolMode::Exclusive,
                panics: false,
                trace: self.clone(),
            }))
            .with(Arc::new(Probe {
                name: BOOM,
                mode: ToolMode::Parallel,
                panics: true,
                trace: self.clone(),
            }))
    }

    fn note(&self, note: Note) {
        self.notes.lock().expect("trace").push(note);
    }

    /// The set of calls in flight after each start, which is every instant at
    /// which overlap could have been created. An end can only shrink the set,
    /// so a rule about overlap is decided at the starts.
    fn instants(&self) -> Vec<BTreeSet<String>> {
        let mut live: BTreeSet<String> = BTreeSet::new();
        let mut seen = Vec::new();
        for note in self.notes.lock().expect("trace").iter() {
            match note {
                Note::Start(id) => {
                    live.insert(id.clone());
                    seen.push(live.clone());
                }
                Note::End(id) => {
                    live.remove(id);
                }
            }
        }
        seen
    }

    /// The call ids in the order their bodies finished.
    fn ends(&self) -> Vec<String> {
        self.notes
            .lock()
            .expect("trace")
            .iter()
            .filter_map(|note| match note {
                Note::End(id) => Some(id.clone()),
                Note::Start(_) => None,
            })
            .collect()
    }
}

/// A tool that records its own overlap, and optionally does not come back.
///
/// The panic happens *between* the start note and the end note on purpose: a
/// containment that lost the end note would still leave the start, so a leaked
/// panic shows up as a call that never finished rather than as a call that was
/// never seen.
struct Probe {
    name: &'static str,
    mode: ToolMode,
    panics: bool,
    trace: Trace,
}

#[async_trait::async_trait]
impl Tool for Probe {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.into(),
            description: "Record when this call starts and ends.".into(),
            parameters: json!({ "type": "object" }),
        }
    }

    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        self.mode
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        let id = arguments["id"].as_str().unwrap_or_default().to_string();
        self.trace.note(Note::Start(id.clone()));

        for _ in 0..arguments["yields"].as_u64().unwrap_or_default() {
            tokio::task::yield_now().await;
        }

        if self.panics {
            panic!("{DELIBERATE}");
        }

        self.trace.note(Note::End(id.clone()));
        Ok(ToolOutcome::ok(id))
    }
}

/// Replace the provider: the first request asks for `calls`, and every later
/// one answers, so the turn ends after the tools have run.
fn asks_for(bus: &EventBus, calls: Vec<ToolCall>) -> EffectHandle {
    let pending = Arc::new(Mutex::new(Some(calls)));
    bus.on_waterfall::<LlmStream, _>(move |_ev, _next| {
        let asked = pending.lock().expect("calls").take().unwrap_or_default();
        Box::pin(async move {
            Ok(ModelResponse {
                content: if asked.is_empty() { "done" } else { "" }.into(),
                tool_calls: asked,
                finish_reason: "stop".into(),
                ..Default::default()
            })
        })
    })
}

fn replay(log_path: &Path) -> Vec<SessionEvent> {
    tetanus_session::replay(log_path).expect("replay")
}

/// The call ids in the order their results were committed.
fn committed(journal: &[SessionEvent]) -> Vec<String> {
    journal
        .iter()
        .filter(|event| event.ty == topic::TOOL_RESULT)
        .map(|event| event.data["call_id"].as_str().unwrap_or_default().into())
        .collect()
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
                .downcast_ref::<String>()
                .is_some_and(|message| message == DELIBERATE);
            if !ours {
                inherited(info);
            }
        }));
    });
}
