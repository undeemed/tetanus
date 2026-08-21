//! Evaluating a parsed program, under a budget it cannot escape.
//!
//! **Every step costs fuel, and every step reads the stop flag.** That is the
//! whole containment story in this process: a `while (true) {}` is stopped
//! because the loop that runs it checks, not because anything outside it can
//! reach in. The clock is read every [`CLOCK_EVERY`] steps rather than every
//! step, because reading it is the most expensive thing a cheap step does.
//!
//! **Output is metered as it is produced.** A program that logs a megabyte a
//! millisecond fails at the cap with the prefix that fitted, rather than
//! after the host has already bought the megabytes.
//!
//! **A value leaves this evaluator only if it is lossless JSON.** Arithmetic
//! is `f64`, so a program can reach infinity in one multiplication; that is
//! not a number JSON has, so it is an `invalid-output` failure and not a
//! `null` nobody asked for.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::types::{Abort, Binding, FailureKind, Namespace};

use super::program::{Expr, Stmt};

/// How many steps between clock readings.
const CLOCK_EVERY: u64 = 512;

/// What a value can be while a program is running. Wider than JSON in one
/// direction - a namespace and a binding are values a program can hold - and
/// wider in another: `Num` is any `f64`, including the ones JSON cannot carry.
#[derive(Clone)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<Val>),
    Map(BTreeMap<String, Val>),
    Namespace(Arc<BTreeMap<String, Binding>>),
    Function { name: String, body: Binding },
    Builtin(&'static str),
}

impl Val {
    /// What this value is, for a message about a value of the wrong kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Val::Null => "null",
            Val::Bool(_) => "a boolean",
            Val::Num(_) => "a number",
            Val::Str(_) => "a string",
            Val::List(_) => "a list",
            Val::Map(_) => "an object",
            Val::Namespace(_) => "a binding namespace",
            Val::Function { .. } | Val::Builtin(_) => "a function",
        }
    }

    /// Truthiness, spelled out rather than inherited from a language: empty
    /// string and zero are false, an empty list is *true*. A program that
    /// wants a length asks for one.
    pub fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(value) => *value,
            Val::Num(value) => *value != 0.0 && !value.is_nan(),
            Val::Str(text) => !text.is_empty(),
            _ => true,
        }
    }

    /// The value as lossless JSON, or why it is not.
    pub fn to_json(&self) -> Result<Value, String> {
        match self {
            Val::Null => Ok(Value::Null),
            Val::Bool(value) => Ok(Value::Bool(*value)),
            // A whole number leaves as a whole number. Arithmetic here is
            // `f64`, so without this every count a program returns would
            // reach the model as `3.0` - which reads as a deliberate choice
            // about precision that nobody made, and which a model then copies
            // into the next thing it writes.
            Val::Num(value) if value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 => {
                Ok(Value::Number(serde_json::Number::from(*value as i64)))
            }
            Val::Num(value) => serde_json::Number::from_f64(*value)
                .map(Value::Number)
                .ok_or_else(|| {
                    format!("{value} is not a number JSON can carry (it is not finite)")
                }),
            Val::Str(text) => Ok(Value::String(text.clone())),
            Val::List(items) => items
                .iter()
                .map(Val::to_json)
                .collect::<Result<Vec<Value>, String>>()
                .map(Value::Array),
            Val::Map(entries) => entries
                .iter()
                .map(|(key, value)| value.to_json().map(|value| (key.clone(), value)))
                .collect::<Result<serde_json::Map<String, Value>, String>>()
                .map(Value::Object),
            Val::Namespace(_) | Val::Function { .. } | Val::Builtin(_) => Err(format!(
                "{} is not a value a program can return",
                self.kind()
            )),
        }
    }

    /// A JSON value as a program value.
    pub fn from_json(value: &Value) -> Val {
        match value {
            Value::Null => Val::Null,
            Value::Bool(value) => Val::Bool(*value),
            Value::Number(number) => Val::Num(number.as_f64().unwrap_or(f64::NAN)),
            Value::String(text) => Val::Str(text.clone()),
            Value::Array(items) => Val::List(items.iter().map(Val::from_json).collect()),
            Value::Object(entries) => Val::Map(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Val::from_json(value)))
                    .collect(),
            ),
        }
    }

    /// The value as a program would print it.
    pub fn render(&self) -> String {
        match self {
            Val::Str(text) => text.clone(),
            Val::Num(value) if value.fract() == 0.0 && value.is_finite() => {
                format!("{}", *value as i64)
            }
            other => other
                .to_json()
                .map(|json| json.to_string())
                .unwrap_or_else(|_| format!("<{}>", other.kind())),
        }
    }
}

/// Why an evaluation stopped early.
pub enum Stop {
    /// The program returned.
    Return(Val),
    /// The program failed: a bad operation, an unknown name, a binding that
    /// said no.
    Failed(String),
    /// A budget ran out. Carries which one.
    Budget(String),
    /// The caller asked for it.
    Aborted,
    /// The output ledger filled up.
    Overflow(String),
}

impl Stop {
    /// The failure class this stop reports as, for a stop that is not a
    /// return.
    pub fn kind(&self) -> FailureKind {
        match self {
            Stop::Return(_) => FailureKind::Exception,
            Stop::Failed(_) => FailureKind::Exception,
            Stop::Budget(_) => FailureKind::Timeout,
            Stop::Aborted => FailureKind::Abort,
            Stop::Overflow(_) => FailureKind::OutputLimit,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Stop::Return(_) => "the program returned".to_string(),
            Stop::Failed(why) | Stop::Budget(why) | Stop::Overflow(why) => why.clone(),
            Stop::Aborted => "the run was aborted".to_string(),
        }
    }
}

/// What one run may spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Evaluation steps. This is the compute budget: it does not move while a
    /// binding the host is running takes its time, which is upstream's rule -
    /// a program is charged for what it does, not for what it waits on.
    pub fuel: u64,
    /// Wall clock for the whole run, binding time included. The backstop for a
    /// program that spends its life inside one slow binding.
    pub wall: Duration,
    /// Logs and completion value together, in bytes.
    pub max_output_bytes: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            // Enough for a program that walks a few thousand items; far too
            // little to sit in a loop unnoticed.
            fuel: 5_000_000,
            wall: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// One run's mutable state.
pub struct Run {
    scopes: Vec<BTreeMap<String, Val>>,
    pub logs: Vec<String>,
    /// Bytes the logs have taken so far.
    logged: usize,
    fuel: u64,
    started: Instant,
    budget: Budget,
    abort: Abort,
    /// Steps since the clock was last read.
    since_clock: u64,
    /// Time spent inside host bindings, which the compute budget does not
    /// charge for.
    binding_time: Duration,
}

impl Run {
    pub fn new(budget: Budget, abort: Abort, bindings: &[Namespace]) -> Self {
        let mut root: BTreeMap<String, Val> = BTreeMap::new();
        for namespace in bindings {
            root.insert(
                namespace.global.clone(),
                Val::Namespace(Arc::new(namespace.functions.clone())),
            );
        }
        for builtin in BUILTINS {
            root.insert((*builtin).to_string(), Val::Builtin(builtin));
        }
        Self {
            scopes: vec![root],
            logs: Vec::new(),
            logged: 0,
            fuel: budget.fuel,
            started: Instant::now(),
            budget,
            abort,
            since_clock: 0,
            binding_time: Duration::ZERO,
        }
    }

    /// How long the run has been going, for the result.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Spend one step, and answer whether the run may continue.
    fn step(&mut self) -> Result<(), Stop> {
        if self.abort.is_stopped() {
            return Err(Stop::Aborted);
        }
        if self.fuel == 0 {
            return Err(Stop::Budget(format!(
                "the program ran past its compute budget of {} steps",
                self.budget.fuel
            )));
        }
        self.fuel -= 1;
        self.since_clock += 1;
        if self.since_clock >= CLOCK_EVERY {
            self.since_clock = 0;
            // Binding time is the caller's, not the program's, so it comes
            // back off the clock before the ceiling is judged.
            if self.started.elapsed().saturating_sub(self.binding_time) > self.budget.wall {
                return Err(Stop::Budget(format!(
                    "the program ran past its wall-clock ceiling of {}ms",
                    self.budget.wall.as_millis()
                )));
            }
        }
        Ok(())
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<&Val> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn declare(&mut self, name: String, value: Val) {
        self.scopes
            .last_mut()
            .expect("a run always has a scope")
            .insert(name, value);
    }

    fn assign(&mut self, name: &str, value: Val) -> Result<(), Stop> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                *slot = value;
                return Ok(());
            }
        }
        Err(Stop::Failed(format!(
            "{name:?} is not defined; declare it with `let` before assigning to it"
        )))
    }

    /// Add one log line, metered.
    fn log(&mut self, line: String) -> Result<(), Stop> {
        let room = self.budget.max_output_bytes.saturating_sub(self.logged);
        if line.len() > room {
            // The prefix that fits is kept: a program that failed by logging
            // too much is usually explained by what it logged first.
            let kept: String = line
                .chars()
                .scan(0usize, |taken, c| {
                    *taken += c.len_utf8();
                    (*taken <= room).then_some(c)
                })
                .collect();
            if !kept.is_empty() {
                self.logs.push(kept);
            }
            self.logged = self.budget.max_output_bytes;
            return Err(Stop::Overflow(format!(
                "the program's output passed the {} byte cap",
                self.budget.max_output_bytes
            )));
        }
        self.logged += line.len();
        self.logs.push(line);
        Ok(())
    }
}

/// The names a program has without any binding at all.
const BUILTINS: &[&str] = &["log", "len", "keys", "str", "num", "push", "floor"];

/// Run a parsed program to its end, or to whatever stopped it.
pub fn run(program: &[Stmt], state: &mut Run) -> Result<Option<Val>, Stop> {
    match block(program, state) {
        Ok(()) => Ok(None),
        Err(Stop::Return(value)) => Ok(Some(value)),
        Err(other) => Err(other),
    }
}

fn block(statements: &[Stmt], state: &mut Run) -> Result<(), Stop> {
    for statement in statements {
        run_statement(statement, state)?;
    }
    Ok(())
}

fn run_statement(statement: &Stmt, state: &mut Run) -> Result<(), Stop> {
    state.step()?;
    match statement {
        Stmt::Let { name, value } => {
            let value = eval(value, state)?;
            state.declare(name.clone(), value);
            Ok(())
        }
        Stmt::Assign { target, value } => {
            let value = eval(value, state)?;
            assign_to(target, value, state)
        }
        Stmt::If { test, then, other } => {
            let taken = eval(test, state)?.truthy();
            state.push_scope();
            let outcome = if taken {
                block(then, state)
            } else {
                block(other, state)
            };
            state.pop_scope();
            outcome
        }
        Stmt::While { test, body } => loop {
            state.step()?;
            if !eval(test, state)?.truthy() {
                return Ok(());
            }
            state.push_scope();
            let outcome = block(body, state);
            state.pop_scope();
            outcome?;
        },
        Stmt::Return(value) => {
            let value = match value {
                Some(expression) => eval(expression, state)?,
                None => Val::Null,
            };
            Err(Stop::Return(value))
        }
        Stmt::Eval(expression) => eval(expression, state).map(|_| ()),
    }
}

/// Write a value into a place.
fn assign_to(target: &Expr, value: Val, state: &mut Run) -> Result<(), Stop> {
    match target {
        Expr::Name(name) => state.assign(name, value),
        Expr::Member { of, name } => {
            let mut holder = eval(of, state)?;
            let Val::Map(entries) = &mut holder else {
                return Err(Stop::Failed(format!(
                    "a member can only be set on an object, not on {}",
                    holder.kind()
                )));
            };
            entries.insert(name.clone(), value);
            assign_to(of, holder, state)
        }
        Expr::Index { of, at } => {
            let index = eval(at, state)?;
            let mut holder = eval(of, state)?;
            match (&mut holder, &index) {
                (Val::List(items), Val::Num(position)) => {
                    let position = *position as usize;
                    if position >= items.len() {
                        return Err(Stop::Failed(format!(
                            "index {position} is past the end of a list of {}",
                            items.len()
                        )));
                    }
                    items[position] = value;
                }
                (Val::Map(entries), Val::Str(key)) => {
                    entries.insert(key.clone(), value);
                }
                (holder, index) => {
                    return Err(Stop::Failed(format!(
                        "{} cannot be indexed by {}",
                        holder.kind(),
                        index.kind()
                    )))
                }
            }
            assign_to(of, holder, state)
        }
        other => Err(Stop::Failed(format!(
            "{other:?} is not a place a value can be written to"
        ))),
    }
}

fn eval(expression: &Expr, state: &mut Run) -> Result<Val, Stop> {
    state.step()?;
    match expression {
        Expr::Number(value) => Ok(Val::Num(*value)),
        Expr::Text(text) => Ok(Val::Str(text.clone())),
        Expr::Bool(value) => Ok(Val::Bool(*value)),
        Expr::Null => Ok(Val::Null),
        Expr::Name(name) => state
            .lookup(name)
            .cloned()
            .ok_or_else(|| Stop::Failed(format!("{name:?} is not defined"))),
        Expr::List(items) => items
            .iter()
            .map(|item| eval(item, state))
            .collect::<Result<Vec<Val>, Stop>>()
            .map(Val::List),
        Expr::Map(entries) => {
            let mut map = BTreeMap::new();
            for (key, value) in entries {
                let value = eval(value, state)?;
                map.insert(key.clone(), value);
            }
            Ok(Val::Map(map))
        }
        Expr::Member { of, name } => {
            let holder = eval(of, state)?;
            member(&holder, name)
        }
        Expr::Index { of, at } => {
            let holder = eval(of, state)?;
            let index = eval(at, state)?;
            match (&holder, &index) {
                (Val::List(items), Val::Num(position)) => {
                    let position = *position;
                    if position < 0.0 || position.fract() != 0.0 {
                        return Err(Stop::Failed(format!(
                            "a list index is a whole number that is not negative, not {position}"
                        )));
                    }
                    items.get(position as usize).cloned().ok_or_else(|| {
                        Stop::Failed(format!(
                            "index {position} is past the end of a list of {}",
                            items.len()
                        ))
                    })
                }
                (Val::Map(_), Val::Str(key)) => member(&holder, key),
                (Val::Str(text), Val::Num(position)) => text
                    .chars()
                    .nth(*position as usize)
                    .map(|c| Val::Str(c.to_string()))
                    .ok_or_else(|| {
                        Stop::Failed(format!("index {position} is past the end of a string"))
                    }),
                (holder, index) => Err(Stop::Failed(format!(
                    "{} cannot be indexed by {}",
                    holder.kind(),
                    index.kind()
                ))),
            }
        }
        Expr::Call { callee, args } => {
            let target = eval(callee, state)?;
            let mut values = Vec::with_capacity(args.len());
            for argument in args {
                values.push(eval(argument, state)?);
            }
            call(target, values, state)
        }
        Expr::Unary { op, of } => {
            let value = eval(of, state)?;
            match (*op, &value) {
                ("-", Val::Num(number)) => Ok(Val::Num(-number)),
                ("-", other) => Err(Stop::Failed(format!("{} cannot be negated", other.kind()))),
                _ => Ok(Val::Bool(!value.truthy())),
            }
        }
        Expr::Binary { op, left, right } => {
            // Short circuit before the right side is evaluated at all, which
            // is what lets `x != null && x.name` be written.
            let head = eval(left, state)?;
            match *op {
                "&&" if !head.truthy() => return Ok(head),
                "||" if head.truthy() => return Ok(head),
                "&&" | "||" => return eval(right, state),
                _ => {}
            }
            let tail = eval(right, state)?;
            binary(op, head, tail)
        }
    }
}

/// One member of an object or a namespace.
fn member(holder: &Val, name: &str) -> Result<Val, Stop> {
    match holder {
        // A name a namespace does not have is the program's mistake, and the
        // message lists what there is: the model can correct itself from it.
        Val::Namespace(functions) => functions
            .get(name)
            .map(|body| Val::Function {
                name: name.to_string(),
                body: Arc::clone(body),
            })
            .ok_or_else(|| {
                Stop::Failed(format!(
                    "this namespace has no {name:?}; it has: {}",
                    functions
                        .keys()
                        .cloned()
                        .collect::<Vec<String>>()
                        .join(", ")
                ))
            }),
        // An absent key is null rather than a failure, so a program can test
        // for one. An absent *namespace member* is not, because that is a
        // call the program was about to make.
        Val::Map(entries) => Ok(entries.get(name).cloned().unwrap_or(Val::Null)),
        other => Err(Stop::Failed(format!(
            "{} has no members to read",
            other.kind()
        ))),
    }
}

/// Call a binding or a builtin.
fn call(target: Val, args: Vec<Val>, state: &mut Run) -> Result<Val, Stop> {
    match target {
        Val::Function { name, body } => {
            let argument = match args.len() {
                0 => Value::Null,
                1 => args[0].to_json().map_err(|why| {
                    Stop::Failed(format!(
                        "the argument to {name:?} is not lossless JSON: {why}"
                    ))
                })?,
                _ => {
                    return Err(Stop::Failed(format!(
                        "{name:?} takes one argument; it was given {}",
                        args.len()
                    )))
                }
            };
            // The clock stops for the host: a binding that takes a second is
            // the caller's second, not the program's compute budget.
            let started = Instant::now();
            let answer = body(&argument);
            state.binding_time += started.elapsed();
            match answer {
                Ok(value) => Ok(Val::from_json(&value)),
                Err(why) => Err(Stop::Failed(format!("{name} failed: {why}"))),
            }
        }
        Val::Builtin(name) => builtin(name, args, state),
        other => Err(Stop::Failed(format!("{} is not callable", other.kind()))),
    }
}

fn builtin(name: &'static str, args: Vec<Val>, state: &mut Run) -> Result<Val, Stop> {
    let first = args.first();
    match name {
        "log" => {
            let line = args
                .iter()
                .map(Val::render)
                .collect::<Vec<String>>()
                .join(" ");
            state.log(line)?;
            Ok(Val::Null)
        }
        "len" => match first {
            Some(Val::Str(text)) => Ok(Val::Num(text.chars().count() as f64)),
            Some(Val::List(items)) => Ok(Val::Num(items.len() as f64)),
            Some(Val::Map(entries)) => Ok(Val::Num(entries.len() as f64)),
            Some(other) => Err(Stop::Failed(format!(
                "len() cannot measure {}",
                other.kind()
            ))),
            None => Err(Stop::Failed("len() takes one argument".to_string())),
        },
        "keys" => match first {
            Some(Val::Map(entries)) => Ok(Val::List(
                entries.keys().map(|key| Val::Str(key.clone())).collect(),
            )),
            Some(other) => Err(Stop::Failed(format!(
                "keys() takes an object, not {}",
                other.kind()
            ))),
            None => Err(Stop::Failed("keys() takes one argument".to_string())),
        },
        "str" => Ok(Val::Str(first.map(Val::render).unwrap_or_default())),
        "num" => match first {
            Some(Val::Num(value)) => Ok(Val::Num(*value)),
            Some(Val::Str(text)) => text
                .trim()
                .parse::<f64>()
                .map(Val::Num)
                .map_err(|_| Stop::Failed(format!("num() cannot read {text:?} as a number"))),
            Some(other) => Err(Stop::Failed(format!("num() cannot read {}", other.kind()))),
            None => Err(Stop::Failed("num() takes one argument".to_string())),
        },
        "push" => match (args.first(), args.get(1)) {
            (Some(Val::List(items)), Some(value)) => {
                let mut grown = items.clone();
                grown.push(value.clone());
                Ok(Val::List(grown))
            }
            _ => Err(Stop::Failed(
                "push() takes a list and a value, and answers a new list".to_string(),
            )),
        },
        "floor" => match first {
            Some(Val::Num(value)) => Ok(Val::Num(value.floor())),
            _ => Err(Stop::Failed("floor() takes a number".to_string())),
        },
        other => Err(Stop::Failed(format!("{other}() is not a builtin"))),
    }
}

fn binary(op: &str, left: Val, right: Val) -> Result<Val, Stop> {
    match (op, &left, &right) {
        ("+", Val::Str(a), b) => Ok(Val::Str(format!("{a}{}", b.render()))),
        ("+", a, Val::Str(b)) => Ok(Val::Str(format!("{}{b}", a.render()))),
        ("+", Val::List(a), Val::List(b)) => {
            let mut joined = a.clone();
            joined.extend(b.iter().cloned());
            Ok(Val::List(joined))
        }
        ("==", a, b) => Ok(Val::Bool(equal(a, b))),
        ("!=", a, b) => Ok(Val::Bool(!equal(a, b))),
        (_, Val::Num(a), Val::Num(b)) => match op {
            "+" => Ok(Val::Num(a + b)),
            "-" => Ok(Val::Num(a - b)),
            "*" => Ok(Val::Num(a * b)),
            "/" if *b == 0.0 => Err(Stop::Failed(
                "a number cannot be divided by zero".to_string(),
            )),
            "/" => Ok(Val::Num(a / b)),
            "%" if *b == 0.0 => Err(Stop::Failed(
                "a number cannot be divided by zero".to_string(),
            )),
            "%" => Ok(Val::Num(a % b)),
            "<" => Ok(Val::Bool(a < b)),
            "<=" => Ok(Val::Bool(a <= b)),
            ">" => Ok(Val::Bool(a > b)),
            ">=" => Ok(Val::Bool(a >= b)),
            other => Err(Stop::Failed(format!("{other} is not an operator"))),
        },
        (_, Val::Str(a), Val::Str(b)) if matches!(op, "<" | "<=" | ">" | ">=") => {
            Ok(Val::Bool(match op {
                "<" => a < b,
                "<=" => a <= b,
                ">" => a > b,
                _ => a >= b,
            }))
        }
        (op, a, b) => Err(Stop::Failed(format!(
            "{} {op} {} is not something this language does",
            a.kind(),
            b.kind()
        ))),
    }
}

/// Equality by value. Two numbers are equal by their `f64` comparison, so
/// `NaN` is equal to nothing including itself, exactly as it is everywhere.
fn equal(left: &Val, right: &Val) -> bool {
    match (left, right) {
        (Val::Null, Val::Null) => true,
        (Val::Bool(a), Val::Bool(b)) => a == b,
        (Val::Num(a), Val::Num(b)) => a == b,
        (Val::Str(a), Val::Str(b)) => a == b,
        (Val::List(a), Val::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Val::Map(a), Val::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| equal(value, other)))
        }
        _ => false,
    }
}
