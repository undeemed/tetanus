//! Asking the user something, and recording what they said.
//!
//! Contract section 4.4.3 settled this shape before anything was built against
//! it; this serves the engine half. A tool that needs a decision only a person
//! can make - which of two approaches, what the missing value is - puts a
//! question here and waits.
//!
//! The rules are the contract's, and each one is a defect if it is quietly
//! relaxed.
//!
//! **An answer covers every question, or it is not an answer.** A tool that
//! asked three things and was given two is in a state its author never wrote
//! code for, so a partial answer is treated as no answer at all. A tool
//! therefore meets one of exactly two cases.
//!
//! **An answer outside a closed list is not an answer.** A question that offers
//! options accepts those labels and nothing else, because the label is both the
//! text and the value. A single-select question given several labels is
//! unanswered rather than first-wins: a tool acting on a guess about which one
//! the user meant is worse than a tool told it has none.
//!
//! **The pair is durable and turn-enclosed.** `question/asked` and
//! `question/answered` share an `id`, one pair per ask, inside the turn - the
//! same enclosure rule [`crate::approval`] follows, for the same reason: the
//! turn is what crash repair closes, so a question outside one could never be
//! closed. A transcript that shows what a tool did but not what the user was
//! asked cannot explain it.
//!
//! **An interrupt is the only way out.** There is no timeout: a person may
//! reasonably take a long time, and an engine that gave up would produce a tool
//! failure that looks like the user's fault. So the interrupt withdraws the
//! question at once, and a late answer is discarded.
//!
//! Parity: upstream `packages/interaction/user-questions` and its consumer
//! `packages/interaction/tool-ask-user`.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::json;
use tetanus_core::events::{DispatchMode, Event};
use tetanus_core::EventBus;
use tetanus_session::{SessionError, SessionLog};

use crate::approval::has_open_turn;
use crate::interrupt::Interrupt;
use crate::log::topic;
use crate::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// One choice offered for a question.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuestionOption {
    /// Both the text shown and the value accepted. One field, deliberately:
    /// a separate value would be a second thing to get out of step with the
    /// label a person actually read.
    pub label: String,
    /// One sentence on what choosing it means, for a surface that can show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl QuestionOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
        }
    }

    pub fn describing(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// One question put to the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Question {
    /// The asker's own id, echoed in the answer. Stable so a tool that asked
    /// three things can tell the three answers apart without relying on order.
    pub id: String,
    /// The question itself.
    pub question: String,
    /// A short heading a surface may group by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The closed list of acceptable answers. Empty means free text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// Whether more than one option may be chosen.
    #[serde(default)]
    pub multi_select: bool,
}

impl Question {
    pub fn new(id: impl Into<String>, question: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            header: None,
            options: Vec::new(),
            multi_select: false,
        }
    }

    pub fn offering<I, O>(mut self, options: I) -> Self
    where
        I: IntoIterator<Item = O>,
        O: Into<QuestionOption>,
    {
        self.options = options.into_iter().map(Into::into).collect();
        self
    }

    pub fn multi(mut self) -> Self {
        self.multi_select = true;
        self
    }

    /// Whether this question accepts anything at all.
    pub fn free_text(&self) -> bool {
        self.options.is_empty()
    }
}

impl From<&str> for QuestionOption {
    fn from(label: &str) -> Self {
        Self::new(label)
    }
}

/// What the user said about one question.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    /// The question this answers.
    pub id: String,
    /// The labels chosen, for a question that offered options.
    #[serde(default)]
    pub selected: Vec<String>,
    /// Free text, for a question that offered none - or beside a selection,
    /// which is how "something else" is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

impl Answer {
    pub fn choosing(
        id: impl Into<String>,
        labels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            selected: labels.into_iter().map(Into::into).collect(),
            custom: None,
        }
    }

    pub fn saying(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            selected: Vec::new(),
            custom: Some(text.into()),
        }
    }

    /// Whether this answer says anything. An entry with nothing chosen and no
    /// text is a client having answered the shape of the question rather than
    /// the question.
    fn says_something(&self) -> bool {
        !self.selected.is_empty()
            || self
                .custom
                .as_ref()
                .is_some_and(|text| !text.trim().is_empty())
    }
}

/// `user/ask` is the answerer seam: a surface answers the questions, or the
/// chain falls through to the terminal, which answers nothing.
///
/// The terminal is `None`, so a bus with no listener leaves the tool told it
/// has no answer rather than hanging or inventing one.
pub struct AskUser {
    /// The id the pair is audited under, so a surface can name the journal
    /// line the prompt it is showing belongs to.
    pub id: String,
    pub questions: Vec<Question>,
}

impl Event for AskUser {
    const TOPIC: &'static str = "user/ask";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = Option<Vec<Answer>>;
}

#[derive(Debug, thiserror::Error)]
pub enum QuestionError {
    /// The same enclosure rule the approval pair follows, for the same reason:
    /// the turn is what crash repair closes, so a question written outside one
    /// could never be closed.
    #[error(
        "questions asked outside an open turn: the question/asked and question/answered pair \
         must be enclosed by the turn that needs the answer, because a turn is what crash repair \
         closes"
    )]
    NoOpenTurn,
    /// The asker built a question nothing could answer. Refused before
    /// anything is written, so no surface is ever shown a prompt a valid answer
    /// does not exist for.
    #[error("the questions cannot be asked: {0}")]
    Malformed(String),
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// Puts questions to whoever is listening, and records both halves.
pub struct QuestionService {
    bus: EventBus,
    log: Arc<dyn SessionLog>,
    /// Shared with the turn that runs the tool doing the asking, so an
    /// interrupt withdraws an outstanding question rather than leaving the
    /// turn waiting on an answer it would not use.
    interrupt: Arc<Interrupt>,
    next: AtomicU64,
    minted: u64,
}

impl QuestionService {
    pub fn new(bus: EventBus, log: Arc<dyn SessionLog>, interrupt: Arc<Interrupt>) -> Arc<Self> {
        // Seeded past the asks the journal already carries, so a resumed
        // session cannot mint an id its own log already uses: the pair is
        // matched by id, and a reused one would make two questions read as one.
        let already = log
            .events()
            .iter()
            .filter(|event| event.ty == topic::QUESTION_ASKED)
            .count() as u64;
        let minted = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();
        Arc::new(Self {
            bus,
            log,
            interrupt,
            next: AtomicU64::new(already),
            minted,
        })
    }

    /// Put one set of questions, and record what came back.
    ///
    /// `None` is "no answer", and it is the same answer for every way of not
    /// getting one: nobody listening, a partial answer, an answer outside a
    /// question's options, a panicking answerer, or an interrupt. They are one
    /// outcome because a tool can do exactly one thing about all of them.
    pub async fn ask(
        &self,
        questions: Vec<Question>,
    ) -> Result<Option<Vec<Answer>>, QuestionError> {
        if !has_open_turn(&self.log.events()) {
            return Err(QuestionError::NoOpenTurn);
        }
        check(&questions)?;

        let id = self.fresh_id();
        self.log.append(
            topic::QUESTION_ASKED,
            json!({ "id": id, "questions": questions }),
        )?;

        let answers = self.put(id.clone(), &questions).await;

        self.log.append(
            topic::QUESTION_ANSWERED,
            json!({
                "id": id,
                "answers": answers.clone().unwrap_or_default(),
                "answered": answers.is_some(),
            }),
        )?;
        Ok(answers)
    }

    /// Dispatch one ask, race it against the interrupt, and judge what comes
    /// back.
    async fn put(&self, id: String, questions: &[Question]) -> Option<Vec<Answer>> {
        // A question nobody is waiting for any more is not put at all.
        if self.interrupt.stopped() {
            return None;
        }
        let mut ask = AskUser {
            id,
            questions: questions.to_vec(),
        };
        let answered = self.bus.waterfall(&mut ask, unanswered());

        let given = tokio::select! {
            biased;
            given = contained(answered) => given,
            // The interrupt withdraws the question. A late answer is discarded
            // by construction: this returns without it, and the journal has the
            // record before anything could observe one.
            _ = self.interrupt.cancelled() => None,
        }?;
        judge(questions, given)
    }

    fn fresh_id(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        format!("ask-{}-{n}", self.minted)
    }
}

/// Whether this set of questions can be answered at all.
///
/// Checked before anything is written, so a surface is never shown a prompt for
/// which no valid answer exists and a journal never carries an ask that could
/// only ever be closed as unanswered.
fn check(questions: &[Question]) -> Result<(), QuestionError> {
    let malformed = |why: String| Err(QuestionError::Malformed(why));
    if questions.is_empty() {
        return malformed("there are none".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for question in questions {
        if question.id.trim().is_empty() {
            return malformed("a question has no id, and an answer names the id it answers".into());
        }
        if !ids.insert(question.id.as_str()) {
            return malformed(format!(
                "two questions share the id {:?}, so one answer would settle both",
                question.id
            ));
        }
        if question.question.trim().is_empty() {
            return malformed(format!("question {:?} asks nothing", question.id));
        }
        let mut labels = std::collections::BTreeSet::new();
        for option in &question.options {
            if option.label.trim().is_empty() {
                return malformed(format!(
                    "question {:?} offers an unlabelled option",
                    question.id
                ));
            }
            if !labels.insert(option.label.as_str()) {
                return malformed(format!(
                    "question {:?} offers {:?} twice, so an answer could not say which was meant",
                    question.id, option.label
                ));
            }
        }
        if question.multi_select && question.free_text() {
            return malformed(format!(
                "question {:?} allows several answers but offers no options to choose between",
                question.id
            ));
        }
    }
    Ok(())
}

/// Judge what an answerer said against what was asked.
///
/// The three rules of contract section 4.4.3, in one place so no caller
/// reimplements two of them: every question answered, every selection a label
/// that was offered, and one selection unless the question said otherwise. An
/// answer naming a question nobody asked is dropped rather than refused - the
/// questions are the contract, and a client that answered more has not
/// answered less.
pub fn judge(questions: &[Question], given: Vec<Answer>) -> Option<Vec<Answer>> {
    let mut kept = Vec::with_capacity(questions.len());
    for question in questions {
        let answer = given.iter().find(|answer| answer.id == question.id)?;
        if !answer.says_something() {
            return None;
        }
        if !question.free_text() {
            let offered = |label: &String| question.options.iter().any(|o| &o.label == label);
            if !answer.selected.iter().all(offered) {
                return None;
            }
            if answer.selected.is_empty() {
                return None;
            }
            if !question.multi_select && answer.selected.len() > 1 {
                return None;
            }
        }
        kept.push(answer.clone());
    }
    Some(kept)
}

/// The terminal of the answerer chain: nobody answered.
fn unanswered() -> tetanus_core::events::Terminal<AskUser> {
    Arc::new(|_ask: &mut AskUser| Box::pin(async move { None }))
}

/// Contain an answerer that panics as "no answer".
///
/// The bus keeps `waterfall` loud on purpose, and this is the same exception
/// [`crate::approval`] takes for the same reason: a question that cannot be
/// answered has a defined outcome, and letting the panic unwind would fail the
/// *turn* instead of leaving the *tool* without an answer.
async fn contained(
    body: impl std::future::Future<Output = Option<Vec<Answer>>>,
) -> Option<Vec<Answer>> {
    match AssertUnwindSafe(body).catch_unwind().await {
        Ok(answers) => answers,
        Err(payload) => {
            let fault = crate::tools::panicked(payload);
            tracing::error!(%fault, "a question answerer panicked");
            None
        }
    }
}

/// The model-facing tool: ask the user, wait, and hand the answer back as an
/// ordinary tool result.
///
/// Registered by a deployment that has somebody to ask. A headless one that
/// registers it anyway is not broken: every ask settles unanswered, and the
/// model is told so in a sentence it can act on.
pub struct AskUserTool {
    questions: Arc<QuestionService>,
}

impl AskUserTool {
    pub const NAME: &'static str = "ask_user_question";

    pub fn new(questions: Arc<QuestionService>) -> Arc<Self> {
        Arc::new(Self { questions })
    }
}

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Ask the user a concise question when you need confirmation, a choice, \
                          or missing information before continuing. Each question needs a stable \
                          id, which is echoed in the answer."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "The questions to put to the user.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Stable id for this question, echoed in the \
                                                    answer.",
                                },
                                "question": {
                                    "type": "string",
                                    "description": "The question to ask.",
                                },
                                "header": {
                                    "type": "string",
                                    "description": "Optional short heading, such as \"Confirm\".",
                                },
                                "options": {
                                    "type": "array",
                                    "description": "Optional choices. The label is the answer: a \
                                                    question that offers options accepts nothing \
                                                    else.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" },
                                        },
                                        "required": ["label"],
                                    },
                                },
                                "multi_select": {
                                    "type": "boolean",
                                    "description": "Whether more than one option may be chosen. \
                                                    Defaults to false.",
                                },
                            },
                            "required": ["id", "question"],
                        },
                    },
                },
                "required": ["questions"],
            }),
        }
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        let questions: Vec<Question> =
            serde_json::from_value(arguments.get("questions").cloned().unwrap_or(json!(null)))
                .map_err(|e| {
                    ToolError::InvalidArguments(
                        Self::NAME.into(),
                        format!("`questions` must be a list of questions: {e}"),
                    )
                })?;

        match self.questions.ask(questions).await {
            // A malformed ask is the model's mistake and is told to the model,
            // not raised as a tool failure: it can fix the shape and ask again.
            Err(QuestionError::Malformed(why)) => Ok(ToolOutcome::failed(format!(
                "The questions were not put to the user because {why}. Fix them and ask again."
            ))),
            Err(other) => Err(ToolError::Failed(Self::NAME.into(), other.to_string())),
            Ok(Some(answers)) => Ok(ToolOutcome::ok(
                serde_json::to_string(&json!({ "answers": answers }))
                    .unwrap_or_else(|_| "{\"answers\":[]}".to_string()),
            )),
            Ok(None) => Ok(ToolOutcome::failed(
                "The user did not answer: nobody was available, the answer did not cover every \
                 question, or the turn is stopping. Continue without it, or say what you need."
                    .to_string(),
            )),
        }
    }
}
