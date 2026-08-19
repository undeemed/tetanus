//! Test Design Specification: transactional boot.
//!
//! Feature under test: `Registry::start_all` rolling a failed pass back, and
//! `Registry::stop_all` unmounting in the reverse of the start order.
//!
//! Approach: plugins that record what ran, one of which refuses to start and
//! one of which refuses to stop. The topological order itself is fixed by
//! `crates/turn/tests/boot.rs`; these cases are about what happens when a
//! mount or an unmount fails.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tetanus_core::effects::EffectError;
use tetanus_core::{Context, Plugin, PluginId, Registry, RegistryError};

type Trace = Arc<Mutex<Vec<String>>>;

/// TC-PLUGIN-1: a plugin that fails to start rolls the pass back.
///
/// Input: three plugins in a dependency chain, where the last one to mount
/// refuses.
/// Expected: `start_all` reports the refusing plugin, and the two already
/// mounted are stopped, dependents before dependencies. A half-mounted registry
/// would leave the caller holding an error and the process holding services
/// nobody owns.
#[test]
fn a_plugin_that_fails_to_start_rolls_the_pass_back() {
    let trace = trace();
    let mut registry = Registry::new();
    registry.insert(plugin(&trace, "base", &[], None)).unwrap();
    registry
        .insert(plugin(&trace, "middle", &["base"], None))
        .unwrap();
    registry
        .insert(plugin(&trace, "late", &["middle"], Some("no provider")))
        .unwrap();

    let error = registry
        .start_all(&mut Context::new())
        .expect_err("the last plugin refuses");

    assert!(
        matches!(&error, RegistryError::Start { id, .. } if id == &PluginId::from("late")),
        "{error}"
    );
    assert_eq!(
        seen(&trace),
        vec![
            "start base",
            "start middle",
            "start late",
            "stop middle",
            "stop base"
        ]
    );
}

/// TC-PLUGIN-2: `stop_all` unmounts dependents before dependencies, and one
/// refusal does not strand the rest.
///
/// Input: a two-plugin chain where the dependent refuses to stop.
/// Expected: both plugins are asked to stop, dependent first, and the refusal
/// comes back named rather than thrown. Stopping at the first fault would leak
/// every plugin behind it, which is the same rule an effect scope follows.
#[test]
fn stop_all_unmounts_dependents_first_and_reports_refusals() {
    let trace = trace();
    let mut registry = Registry::new();
    registry.insert(plugin(&trace, "base", &[], None)).unwrap();
    let mut dependent = plugin(&trace, "top", &["base"], None);
    dependent.stop_fault = Some("still busy");
    registry.insert(dependent).unwrap();

    let faults = registry.stop_all().expect("the graph resolves");

    assert_eq!(seen(&trace), vec!["stop top", "stop base"]);
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].0, PluginId::from("top"));
    assert!(
        faults[0].1.to_string().contains("still busy"),
        "{}",
        faults[0].1
    );
}

/// A plugin that records every call and answers as told: a fault is the reason
/// it refuses, and `None` is a plugin that does as it is asked.
struct Recorded {
    id: PluginId,
    deps: Vec<PluginId>,
    trace: Trace,
    start_fault: Option<&'static str>,
    stop_fault: Option<&'static str>,
}

impl Plugin for Recorded {
    fn id(&self) -> PluginId {
        self.id.clone()
    }
    fn deps(&self) -> Vec<PluginId> {
        self.deps.clone()
    }
    fn start(&self, _ctx: &mut Context) -> Result<(), EffectError> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("start {}", self.id.0));
        refuse(self.start_fault)
    }
    fn stop(&self) -> Result<(), EffectError> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("stop {}", self.id.0));
        refuse(self.stop_fault)
    }
}

fn plugin(
    trace: &Trace,
    id: &str,
    deps: &[&str],
    start_fault: Option<&'static str>,
) -> Box<Recorded> {
    Box::new(Recorded {
        id: PluginId::from(id),
        deps: deps.iter().copied().map(PluginId::from).collect(),
        trace: Arc::clone(trace),
        start_fault,
        stop_fault: None,
    })
}

fn refuse(fault: Option<&'static str>) -> Result<(), EffectError> {
    match fault {
        Some(why) => Err(EffectError::Failed(why.to_string())),
        None => Ok(()),
    }
}

fn trace() -> Trace {
    Arc::new(Mutex::new(Vec::new()))
}

fn seen(trace: &Trace) -> Vec<String> {
    trace.lock().unwrap().clone()
}
