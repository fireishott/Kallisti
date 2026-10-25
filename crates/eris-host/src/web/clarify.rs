//! The clarify tool and the gate that blocks a turn until it is answered.
//!
//! A model that has to ask a question had one way to do it: end the turn with a
//! question in the reply and wait for the operator to type. That works, and it is
//! what [`crate::persona::PersonaConfig::ASKING`] still says for a turn with no
//! tools, but it has two costs. The turn is over, so answering means the model
//! re-reads the whole prompt to continue; and the question is prose, so the
//! operator retypes something the daemon could have offered as three buttons.
//!
//! This module is the other way: a tool the model calls, which parks the turn the
//! way an approval parks a call, waits for a decision on the turn's own blocking
//! thread, and resumes with the answer as the tool result. The turn never ends, so
//! the model continues from the exact state it asked in.
//!
//! Two pieces:
//!
//! - The wire types: [`Request`], [`Choice`], [`Answer`], and the tool spec the
//!   model is offered. Kept apart from the waiting so a device client, the
//!   dashboard and a test all parse the same shape.
//! - [`Gate`], a handle the turn holds while it waits. It owns no database and
//!   knows no transport: settling it is whoever can reach the same key.
//!
//! The waiting is a condition variable with a deadline, not a poll and not a
//! sleep. The turn is already on a dedicated blocking thread, so a condvar costs
//! one parked thread that is doing nothing else anyway, and it wakes the instant a
//! decision lands rather than up to a second later.
//!
//! A turn whose operator walked away must not hold the conversation forever, so a
//! wait that reaches its deadline answers with [`Answer::timed_out`] and the model
//! is told to proceed on its own judgement. That is a sentence in a tool result,
//! not a failure: the turn carries on and the operator reads what it decided.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use core_runtime::provider::ToolSpec;

/// The tool name the model calls.
pub const TOOL: &str = "clarify";

/// How long a turn waits for an answer before proceeding on its own.
///
/// An hour by default. Long enough that a question asked on the way out of the
/// office is answerable when the operator comes back, short enough that a turn
/// nobody is ever going to answer does not hold a lease until the process
/// restarts. A `0` disables the wait entirely, which is what a surface with no way
/// to answer should use: see [`crate::web::chat::GateDecision::Refuse`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The most choices one question may offer.
///
/// Four, because the composer renders them as buttons in a row and a fifth turns
/// the row into a paragraph. The model is told the limit in the tool description;
/// a call that sends more is refused rather than truncated, because silently
/// dropping the option the operator wanted is worse than saying the question is
/// malformed.
pub const MAX_CHOICES: usize = 4;

/// The most questions one call may batch.
pub const MAX_QUESTIONS: usize = 5;

/// One option the operator can pick.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Choice {
    /// What the button says. Kept short by the schema, not by truncation here.
    pub label: String,
    /// What picking it means, for a model that needs more than the label.
    pub detail: Option<String>,
}

/// One question and its options.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Question {
    /// The question itself.
    pub question: String,
    /// The options, in the order they should be shown.
    pub choices: Vec<Choice>,
    /// Whether more than one may be picked.
    pub multi: bool,
}

/// What the model asked for, as one call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    /// A one-line headline for the card, when the model offered one.
    pub title: Option<String>,
    /// The questions, in order.
    pub questions: Vec<Question>,
}

/// Why a call was refused before anybody was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// No questions.
    Empty,
    /// More than [`MAX_QUESTIONS`].
    TooManyQuestions,
    /// A question with a blank body.
    BlankQuestion,
    /// A question with no usable choices.
    NoChoices,
    /// More than [`MAX_CHOICES`] on one question.
    TooManyChoices,
    /// A choice with a blank label.
    BlankChoice,
}

impl Malformed {
    /// Why it was refused, for the model and for the audit row.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Empty => "clarify was called with no questions",
            Self::TooManyQuestions => "clarify was called with more than five questions",
            Self::BlankQuestion => "one of the clarify questions was blank",
            Self::NoChoices => "one of the clarify questions offered no choices",
            Self::TooManyChoices => "one of the clarify questions offered more than four choices",
            Self::BlankChoice => "one of the clarify choices had a blank label",
        }
    }
}

impl Request {
    /// Read one call's arguments.
    ///
    /// Accepts both the batch shape - `questions: [...]` - and the single-question
    /// shape with `question` and `choices` at the top level, because a model that
    /// was told about `questions` still writes the shorter one about half the
    /// time, and refusing that would be a tool that fails on a well-meant call.
    ///
    /// # Errors
    ///
    /// Whatever made the call unanswerable: see [`Malformed`].
    pub fn parse(arguments: &serde_json::Value) -> Result<Self, Malformed> {
        let title = arguments
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);

        let raw: Vec<&serde_json::Value> = match arguments.get("questions") {
            Some(serde_json::Value::Array(items)) if !items.is_empty() => items.iter().collect(),
            Some(serde_json::Value::Array(_)) => return Err(Malformed::Empty),
            _ => vec![arguments],
        };
        if raw.len() > MAX_QUESTIONS {
            return Err(Malformed::TooManyQuestions);
        }

        let mut questions = Vec::with_capacity(raw.len());
        for item in raw {
            let question = item
                .get("question")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or(Malformed::BlankQuestion)?;
            let choices: Vec<Choice> = item
                .get("choices")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|choice| {
                            // A string choice is the commonest shape and means the
                            // same as a label with no detail.
                            let (label, detail) = match choice {
                                serde_json::Value::String(label) => (label.clone(), None),
                                value => (
                                    value
                                        .get("label")
                                        .and_then(serde_json::Value::as_str)?
                                        .to_owned(),
                                    value
                                        .get("detail")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned),
                                ),
                            };
                            Some(Choice {
                                label: label.trim().to_owned(),
                                detail,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if choices.is_empty() {
                return Err(Malformed::NoChoices);
            }
            if choices.len() > MAX_CHOICES {
                return Err(Malformed::TooManyChoices);
            }
            if choices.iter().any(|choice| choice.label.is_empty()) {
                return Err(Malformed::BlankChoice);
            }
            questions.push(Question {
                question: question.to_owned(),
                choices,
                multi: item
                    .get("multi_select")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            });
        }
        if questions.is_empty() {
            return Err(Malformed::Empty);
        }
        Ok(Self { title, questions })
    }
}

/// What the operator decided.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Answer {
    /// They picked one or more choices, by label, and may have added their own
    /// words.
    Chosen {
        /// The question the answer belongs to.
        question: String,
        /// The labels picked, in the order they were offered.
        labels: Vec<String>,
        /// Anything they typed in addition, when the card collects it.
        note: Option<String>,
    },
    /// They typed an answer that was not a choice.
    Typed {
        /// The question the answer belongs to.
        question: String,
        /// What they wrote.
        text: String,
    },
    /// Nobody answered in time.
    TimedOut {
        /// How long the turn waited.
        waited: Duration,
    },
}

impl Answer {
    /// What the model is told, as the tool result.
    ///
    /// A sentence rather than JSON, because this lands in a prompt beside tool
    /// output and the model reads a sentence more reliably than a nested object.
    /// The timeout carries the instruction as well as the fact: a model told only
    /// "no answer" tends to ask again, which is the one thing a walked-away
    /// operator cannot fix.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Chosen {
                question,
                labels,
                note,
            } => {
                let mut out = format!("The operator answered {question:?} with: ");
                out.push_str(&labels.join(", "));
                if let Some(note) = note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    out.push_str(&format!(". They added: {note}"));
                }
                out.push_str(". Continue with that answer.");
                out
            }
            Self::Typed { question, text } => format!(
                "The operator answered {question:?} with: {}. Continue with that answer.",
                text.trim()
            ),
            Self::TimedOut { waited } => format!(
                "The operator did not answer within {}. Proceed on your own best judgement, \
                 say which way you went and why, and do not ask again.",
                human(*waited)
            ),
        }
    }
}

/// A duration as somebody would say it.
#[must_use]
pub fn human(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0 => "no time".to_owned(),
        secs if secs < 60 => format!("{secs}s"),
        secs if secs < 3600 => format!("{}m", secs / 60),
        secs => {
            let hours = secs / 3600;
            if hours == 1 {
                "an hour".to_owned()
            } else {
                format!("{hours} hours")
            }
        }
    }
}

/// One waiting turn: where its request lives until somebody settles it.
struct Waiter {
    /// The conversation the turn that asked belongs to.
    ///
    /// The answer route is addressed by conversation rather than by an id the page
    /// has to carry: a conversation holds one turn at a time under its lease, so
    /// there is at most one question waiting per conversation, and a page that
    /// reloaded mid-card still knows which conversation it is looking at.
    conversation: String,
    request: Request,
    decision: Option<Answer>,
}

/// Every parked clarify in the process, by id.
///
/// A registry rather than a channel per turn because the thing that answers is an
/// HTTP request on another thread that has only an id in hand. It is keyed by a
/// fresh uuid and never reused, so a settle for a turn that already gave up finds
/// nothing and says so instead of waking somebody else's question.
#[derive(Default)]
pub struct Registry {
    waiting: Mutex<HashMap<String, Waiter>>,
    ready: Condvar,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field(
                "waiting",
                &self.waiting.lock().map(|w| w.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a question and return the handle a turn waits on.
    ///
    /// The row is registered before the turn starts waiting, so an answer that
    /// arrives in the microseconds between the frame and the wait is not lost.
    #[must_use]
    pub fn park(self: &Arc<Self>, request: Request) -> Gate {
        self.park_for("", request)
    }

    /// Park a question for a conversation, which is how a route finds it again.
    #[must_use]
    pub fn park_for(self: &Arc<Self>, conversation: &str, request: Request) -> Gate {
        let id = uuid::Uuid::now_v7().to_string();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert(
                id.clone(),
                Waiter {
                    conversation: conversation.to_owned(),
                    request: request.clone(),
                    decision: None,
                },
            );
        }
        Gate {
            id,
            request,
            registry: Arc::clone(self),
        }
    }

    /// The question behind an id, for a surface that is redrawing a card.
    #[must_use]
    pub fn request(&self, id: &str) -> Option<Request> {
        self.waiting
            .lock()
            .ok()
            .and_then(|waiting| waiting.get(id).map(|waiter| waiter.request.clone()))
    }

    /// Everything still waiting, for a device that reconnects mid-question.
    ///
    /// Sorted by id, which is a time-ordered uuid, so the list is oldest first and
    /// a redrawn page keeps the order it drew in.
    #[must_use]
    pub fn waiting(&self) -> Vec<(String, Request)> {
        let mut out: Vec<(String, Request)> = self
            .waiting
            .lock()
            .map(|waiting| {
                waiting
                    .iter()
                    .map(|(id, waiter)| (id.clone(), waiter.request.clone()))
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|left, right| left.0.cmp(&right.0));
        out
    }

    /// Answer a question.
    ///
    /// Returns `false` when nothing was waiting under that id - already answered,
    /// already timed out, or never there - and changed nothing. A caller that
    /// treats `false` as success would be reporting a decision nobody received.
    pub fn settle(&self, id: &str, answer: Answer) -> bool {
        let Ok(mut waiting) = self.waiting.lock() else {
            return false;
        };
        let Some(waiter) = waiting.get_mut(id) else {
            return false;
        };
        // One-way, exactly like an approval: a second answer finds the first one
        // already there and changes nothing.
        if waiter.decision.is_some() {
            return false;
        }
        waiter.decision = Some(answer);
        self.ready.notify_all();
        true
    }

    /// Answer whatever the given conversation is waiting on.
    ///
    /// `false` when nothing is waiting there, which a route reports rather than
    /// pretending the answer landed.
    pub fn answer(&self, conversation: &str, answer: Answer) -> bool {
        let Ok(waiting) = self.waiting.lock() else {
            return false;
        };
        let Some((id, _)) = waiting
            .iter()
            .find(|(_, waiter)| waiter.conversation == conversation)
        else {
            return false;
        };
        let id = id.clone();
        drop(waiting);
        self.settle(&id, answer)
    }

    /// Take the question a conversation is waiting on, as a handle to wait with.
    ///
    /// This is what a turn calls once the model has asked: the row was parked by
    /// the gate, and taking it here is what turns the parked request into a thread
    /// that blocks.
    #[must_use]
    pub fn take(self: &Arc<Self>, conversation: &str) -> Option<Gate> {
        let waiting = self.waiting.lock().ok()?;
        let (id, waiter) = waiting
            .iter()
            .find(|(_, waiter)| waiter.conversation == conversation)?;
        Some(Gate {
            id: id.clone(),
            request: waiter.request.clone(),
            registry: Arc::clone(self),
        })
    }

    /// The question a conversation is waiting on, as an id and its request.
    ///
    /// A read, not a take: a device that asks "is there a question for this job"
    /// must not consume the question it is asking about. `None` when nothing is
    /// waiting there, which is also the answer for a question that was already
    /// settled or whose turn has ended.
    #[must_use]
    pub fn waiting_for(&self, conversation: &str) -> Option<(String, Request)> {
        self.waiting.lock().ok().and_then(|waiting| {
            waiting
                .iter()
                // A decided row is still in the map: settling records the answer
                // and wakes the thread, and the waiting turn removes the row when
                // it wakes. Between those two moments the question is answered, so
                // a surface asking "is anybody still waiting" must be told no.
                .find(|(_, waiter)| {
                    waiter.conversation == conversation && waiter.decision.is_none()
                })
                .map(|(id, waiter)| (id.clone(), waiter.request.clone()))
        })
    }

    /// Whether anything is waiting under this id.
    #[must_use]
    pub fn is_waiting(&self, id: &str) -> bool {
        self.waiting
            .lock()
            .map(|waiting| waiting.contains_key(id))
            .unwrap_or(false)
    }

    /// Forget a question whose turn is gone.
    fn forget(&self, id: &str) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.remove(id);
        }
    }
}

/// One turn's handle on a parked question.
///
/// Dropping it without settling is how a stopped turn cleans up: the entry is
/// removed, so a card left on a page answers a question nobody is waiting for and
/// is told so rather than appearing to work.
#[derive(Debug)]
pub struct Gate {
    id: String,
    request: Request,
    registry: Arc<Registry>,
}

impl Gate {
    /// The id a surface uses to answer this question.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What was asked.
    #[must_use]
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Block the turn here until somebody answers, or the deadline passes.
    ///
    /// `poll` is called on every wake, because a turn that is waiting is still a
    /// turn the operator can stop: a Stop while a question is on screen has to end
    /// the turn, not wait out the hour.
    ///
    /// Returns `None` when the turn asked to stop. The caller turns that into a
    /// cancelled turn, exactly as it would at any other boundary.
    ///
    /// The wait is a condition variable with a bounded slice. The slice exists so
    /// `poll` runs even when nobody notifies: a Stop writes a flag, not a condvar,
    /// and a turn that only woke on an answer would ignore it for an hour.
    pub fn wait(self, timeout: Duration, mut poll: impl FnMut() -> WaitPoll) -> Option<Answer> {
        let deadline = Instant::now().checked_add(timeout);
        // Held across the wait and re-acquired by `wait_timeout`, so a decision
        // cannot slip between the check and the sleep. Every exit below drops it.
        let mut waiting = self.registry.waiting.lock().ok()?;
        loop {
            let Some(waiter) = waiting.get(&self.id) else {
                // The row was removed under us. Report the wait as over rather
                // than spinning on an entry that is gone.
                return Some(Answer::TimedOut {
                    waited: Duration::ZERO,
                });
            };
            if let Some(decision) = waiter.decision.clone() {
                waiting.remove(&self.id);
                return Some(decision);
            }
            if poll() == WaitPoll::Stop {
                waiting.remove(&self.id);
                return None;
            }
            let now = Instant::now();
            let slice = match deadline {
                Some(deadline) if now >= deadline => {
                    waiting.remove(&self.id);
                    return Some(Answer::TimedOut { waited: timeout });
                }
                Some(deadline) => {
                    Duration::from_millis(500).min(deadline.saturating_duration_since(now))
                }
                None => Duration::from_millis(500),
            };
            // A poisoned condvar means another thread panicked while holding the
            // lock. The wait is still answerable, so the guard is recovered and
            // the loop continues rather than turning a panic elsewhere into a
            // turn that dies here.
            let (guard, _) = match self.registry.ready.wait_timeout(waiting, slice) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            waiting = guard;
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        // Answered, timed out or abandoned, the row goes: nothing may answer a
        // question whose turn has already moved on. Removing an id that `wait`
        // already removed is a no-op, which is what makes this safe on every path.
        self.registry.forget(&self.id);
    }
}

/// What a waiting turn should do about the operator, asked on every wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitPoll {
    /// Keep waiting.
    Continue,
    /// The operator stopped the turn.
    Stop,
}

/// What a turn needs to ask its operator something: the registry and which
/// conversation the turn belongs to.
///
/// One argument rather than two, because they only ever travel together: a
/// registry with no conversation is a question parked where no route will look
/// for it, and a conversation with no registry is a turn that cannot ask at all.
#[derive(Debug, Clone)]
pub struct Ask {
    /// Every parked question in the process.
    pub registry: Arc<Registry>,
    /// The conversation the asking turn belongs to.
    pub conversation: String,
}

impl Ask {
    /// A turn's way of asking.
    #[must_use]
    pub fn new(registry: Arc<Registry>, conversation: impl Into<String>) -> Self {
        Self {
            registry,
            conversation: conversation.into(),
        }
    }
}

/// The tool as the model sees it.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL.to_owned(),
        description: "Ask the operator a question and wait for them to answer it. Use this \
                      instead of ending the turn with a question in prose: the turn stays \
                      open and resumes with their answer as this tool's result. Offer \
                      concrete choices, most useful first, with an optional free-text answer \
                      always available. Do not use it for trivial confirmations or to report \
                      progress."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "A one-line headline for the question card."
                },
                "questions": {
                    "type": "array",
                    "maxItems": MAX_QUESTIONS,
                    "description": "One to five questions, asked as one card.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The question, as one sentence."
                            },
                            "choices": {
                                "type": "array",
                                "maxItems": MAX_CHOICES,
                                "minItems": 1,
                                "description": "The options to offer, most useful first.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "The button text, a few words."
                                        },
                                        "detail": {
                                            "type": "string",
                                            "description": "What picking it means, when the label is not enough."
                                        }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "Whether more than one choice may be picked."
                            }
                        },
                        "required": ["question", "choices"]
                    }
                },
                "question": {
                    "type": "string",
                    "description": "A single question, as an alternative to `questions`."
                },
                "choices": {
                    "type": "array",
                    "maxItems": MAX_CHOICES,
                    "description": "The options for the single question.",
                    "items": {"type": "string"}
                }
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn request() -> Request {
        Request {
            title: Some("Which lane?".to_owned()),
            questions: vec![Question {
                question: "Which lane should this land on?".to_owned(),
                choices: vec![
                    Choice {
                        label: "Web first".to_owned(),
                        detail: None,
                    },
                    Choice {
                        label: "All three".to_owned(),
                        detail: Some("slower".to_owned()),
                    },
                ],
                multi: false,
            }],
        }
    }

    #[test]
    fn the_batch_shape_and_the_single_question_shape_both_parse() {
        let batch = serde_json::json!({
            "title": "Which lane?",
            "questions": [{
                "question": "Which lane should this land on?",
                "choices": ["Web first", {"label": "All three", "detail": "slower"}],
                "multi_select": false
            }]
        });
        let parsed = Request::parse(&batch).expect("parses");
        assert_eq!(parsed, request());

        let single = serde_json::json!({
            "question": "Which lane should this land on?",
            "choices": ["Web first", "All three"]
        });
        let parsed = Request::parse(&single).expect("parses");
        assert_eq!(parsed.questions.len(), 1);
        assert_eq!(parsed.questions[0].choices.len(), 2);
        assert_eq!(parsed.title, None);
    }

    #[test]
    fn a_question_nobody_could_answer_is_refused_by_name() {
        for (arguments, expected) in [
            (serde_json::json!({}), Malformed::BlankQuestion),
            (serde_json::json!({"questions": []}), Malformed::Empty),
            (
                serde_json::json!({"question": "  ", "choices": ["a"]}),
                Malformed::BlankQuestion,
            ),
            (
                serde_json::json!({"question": "which?", "choices": []}),
                Malformed::NoChoices,
            ),
            (
                serde_json::json!({"question": "which?", "choices": ["a", "b", "c", "d", "e"]}),
                Malformed::TooManyChoices,
            ),
            (
                serde_json::json!({"question": "which?", "choices": ["a", " "]}),
                Malformed::BlankChoice,
            ),
        ] {
            assert_eq!(
                Request::parse(&arguments).err(),
                Some(expected),
                "{arguments}"
            );
            assert!(!expected.reason().is_empty());
        }
    }

    #[test]
    fn a_decision_wakes_the_waiting_turn() {
        let registry = Arc::new(Registry::new());
        let gate = registry.park(request());
        let id = gate.id().to_owned();
        assert!(registry.is_waiting(&id));

        let answerer = {
            let registry = Arc::clone(&registry);
            let id = id.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                registry.settle(
                    &id,
                    Answer::Chosen {
                        question: "Which lane should this land on?".to_owned(),
                        labels: vec!["Web first".to_owned()],
                        note: None,
                    },
                )
            })
        };
        let answer = gate
            .wait(Duration::from_secs(5), || WaitPoll::Continue)
            .expect("the turn was not stopped");
        assert!(answerer.join().expect("thread"));
        match answer {
            Answer::Chosen { labels, .. } => assert_eq!(labels, vec!["Web first".to_owned()]),
            other => panic!("unexpected answer: {other:?}"),
        }
        // The row is gone once it is answered, so a second answer changes nothing.
        assert!(!registry.is_waiting(&id));
        assert!(!registry.settle(
            &id,
            Answer::Typed {
                question: "q".to_owned(),
                text: "second".to_owned()
            }
        ));
    }

    #[test]
    fn an_answer_that_lands_before_the_wait_is_not_lost() {
        let registry = Arc::new(Registry::new());
        let gate = registry.park(request());
        let id = gate.id().to_owned();
        // Settled before `wait` is even called, which is the race a frame and a
        // wait in two places would otherwise lose.
        assert!(registry.settle(
            &id,
            Answer::Typed {
                question: "Which lane should this land on?".to_owned(),
                text: "both".to_owned(),
            }
        ));
        let answer = gate
            .wait(Duration::from_secs(1), || WaitPoll::Continue)
            .expect("not stopped");
        assert!(matches!(answer, Answer::Typed { .. }));
    }

    #[test]
    fn an_unanswered_question_times_out_and_says_to_carry_on() {
        let registry = Arc::new(Registry::new());
        let gate = registry.park(request());
        let answer = gate
            .wait(Duration::from_millis(50), || WaitPoll::Continue)
            .expect("not stopped");
        assert!(matches!(answer, Answer::TimedOut { .. }));
        let rendered = answer.render();
        assert!(rendered.contains("did not answer"), "{rendered}");
        assert!(rendered.contains("do not ask again"), "{rendered}");
    }

    #[test]
    fn a_stopped_turn_stops_waiting_and_leaves_nothing_behind() {
        let registry = Arc::new(Registry::new());
        let gate = registry.park(request());
        let id = gate.id().to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let waiter = {
            let stop = Arc::clone(&stop);
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                let gate = registry.park(request());
                let id = gate.id().to_owned();
                let answer = gate.wait(Duration::from_secs(30), || {
                    if stop.load(Ordering::SeqCst) {
                        WaitPoll::Stop
                    } else {
                        WaitPoll::Continue
                    }
                });
                (id, answer)
            })
        };
        // The thread parked its own gate; the outer one is unrelated and is
        // dropped here, which must also clean up.
        drop(gate);
        assert!(!registry.is_waiting(&id), "a dropped gate leaves no row");
        stop.store(true, Ordering::SeqCst);
        let (_, answer) = waiter.join().expect("thread");
        assert!(answer.is_none(), "a stopped turn gets no answer");
    }

    #[test]
    fn a_waiting_registry_can_be_listed_for_a_reconnecting_page() {
        let registry = Arc::new(Registry::new());
        let first = registry.park(request());
        let second = registry.park(Request {
            title: None,
            questions: vec![Question {
                question: "And then?".to_owned(),
                choices: vec![Choice {
                    label: "Stop".to_owned(),
                    detail: None,
                }],
                multi: false,
            }],
        });
        let listed = registry.waiting();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0].0,
            *first.id(),
            "the list is oldest first, so a redrawn card keeps its order"
        );
        assert_eq!(listed[1].0, *second.id());
        assert_eq!(
            registry.request(first.id()).map(|request| request.title),
            Some(Some("Which lane?".to_owned()))
        );
    }

    #[test]
    fn a_question_already_answered_is_not_still_waiting() {
        let registry = Arc::new(Registry::new());
        let gate = registry.park_for("job-1", request());
        assert!(registry.waiting_for("job-1").is_some());
        assert!(registry.settle(
            gate.id(),
            Answer::Typed {
                question: "Which lane should this land on?".to_owned(),
                text: "web".to_owned(),
            }
        ));
        // Settled, and the parked thread has not woken yet. A surface that asked
        // now must be told nobody is waiting, or it draws a card whose buttons
        // change nothing.
        assert!(
            registry.waiting_for("job-1").is_none(),
            "a decided question is not a waiting one"
        );
        assert!(registry.waiting_for("job-2").is_none());
    }

    #[test]
    fn the_spec_tells_the_model_not_to_end_the_turn_with_a_question() {
        let spec = spec();
        assert_eq!(spec.name, "clarify");
        assert!(spec.description.contains("wait for them to answer"));
        assert!(
            spec.description.contains("instead of ending the turn"),
            "the point of the tool is that the turn stays open"
        );
        assert_eq!(
            spec.parameters["properties"]["questions"]["maxItems"],
            serde_json::json!(MAX_QUESTIONS)
        );
    }

    #[test]
    fn a_typed_answer_and_a_choice_read_differently_to_the_model() {
        let chosen = Answer::Chosen {
            question: "Which lane?".to_owned(),
            labels: vec!["Web first".to_owned(), "Then device".to_owned()],
            note: Some("ship 106 first".to_owned()),
        };
        let rendered = chosen.render();
        assert!(rendered.contains("Web first, Then device"), "{rendered}");
        assert!(rendered.contains("ship 106 first"), "{rendered}");
        assert!(rendered.ends_with("Continue with that answer."));

        let typed = Answer::Typed {
            question: "Which lane?".to_owned(),
            text: " web, then ios".to_owned(),
        };
        assert!(typed.render().contains("web, then ios"));
    }
}
