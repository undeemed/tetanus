//! Test Design Specification: what the provider stream decoder guarantees for
//! every stream, not for one recorded one.
//!
//! Feature under test: [`tetanus_turn::llm::deepseek::StreamDecoder`], which
//! folds the frames of a provider stream into the chunks a surface renders and
//! the one response the turn records. Upstream pins the same invariants over
//! its `BlockAssembler` with fast-check in
//! `packages/llm/llm/tests/properties.spec.ts`.
//!
//! Approach: model based. A case generates a list of frames as data, renders
//! them onto the wire, and computes the answer they should fold to from the
//! same list, so the assertion is against a second statement of the rule
//! rather than against the decoder's own output. The generators cover
//! fragmented tool calls, arguments that never become JSON, a repeated finish
//! reason and text after the sentinel, because those are the folds a
//! recorded stream fixes to one shape.
//!
//! Features NOT tested here: the SSE framing that finds the payloads
//! (`take_frames`), how the adapter treats an answer that says nothing, and
//! every recorded-stream case - all in `crates/turn/tests/deepseek_adapter.rs`
//! and `upstream_deepseek_wire.rs`. This file only pins what must hold for
//! every stream.
//!
//! Environmental needs: none. No case reaches a network, a key or a file.
//!
//! Pass criteria: each case's stated expected result holds for every
//! generated stream.
//! Fail criteria: any counterexample, or a panic.

use std::collections::BTreeMap;

use proptest::prelude::*;
use tetanus_turn::llm::deepseek::{StreamDecoder, DEFAULT_FINISH_REASON};
use tetanus_turn::llm::{ModelResponse, StreamChunk};
use tetanus_turn::tools::ToolCall;

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// TC-PROP-STREAM-1: a stream yields one tool call per index it named, and
    /// no more.
    ///
    /// Input: any frame list, including lists that split one call across
    /// several frames and lists that name the same index again later.
    /// Expected: the response holds one call per distinct index the accepted
    /// frames named, in ascending index order, with the id, the name and the
    /// arguments the frames built; and the chunks `finish` emits are those
    /// same calls. A decoder that kept a partial per frame rather than per
    /// index would answer with more calls than the model asked for.
    #[test]
    fn one_tool_call_per_index_named(frames in frames()) {
        let (chunks, response) = decode(&frames);
        let expected = fold(&frames);

        prop_assert_eq!(response.tool_calls.len(), expected.calls.len());
        prop_assert_eq!(&response.tool_calls, &expected.tool_calls());
        let indices: Vec<u64> = expected.calls.keys().copied().collect();
        let mut ascending = indices.clone();
        ascending.sort_unstable();
        prop_assert_eq!(indices, ascending, "the calls come out in index order");

        let called: Vec<&ToolCall> = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::ToolCall { call } => Some(call),
                _ => None,
            })
            .collect();
        prop_assert_eq!(called.len(), response.tool_calls.len());
        for (chunk, answered) in called.iter().zip(&response.tool_calls) {
            prop_assert_eq!(*chunk, answered, "a call is streamed as it is answered");
        }
    }

    /// TC-PROP-STREAM-2: the answer restates the stream a surface has already
    /// rendered.
    ///
    /// Input: any frame list.
    /// Expected: `content` is the concatenation of the text deltas the pushes
    /// emitted and `reasoning` is the concatenation of the reasoning deltas,
    /// so a surface that rendered the chunks and then reads the answer is not
    /// shown two different messages. An empty delta is no chunk at all.
    #[test]
    fn the_answer_is_the_chunks_it_streamed(frames in frames()) {
        let (streamed, response) = pushed(&frames);
        let text: String = streamed
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::Text { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        let reasoning: String = streamed
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::Reasoning { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();

        prop_assert_eq!(&response.content, &text);
        prop_assert_eq!(&response.reasoning, &reasoning);
        prop_assert!(
            streamed.iter().all(|chunk| !matches!(chunk, StreamChunk::Text { delta } if delta.is_empty())),
            "an empty delta is not a chunk",
        );
    }

    /// TC-PROP-STREAM-3: the fold is a function of the frames alone.
    ///
    /// Input: any frame list, decoded twice by two decoders.
    /// Expected: the two answers are equal. Upstream states this as `blocks()`
    /// being stable across repeated calls; `finish` consumes the decoder here,
    /// so calling it twice is unrepresentable and what is left to pin is that
    /// nothing outside the frames reaches the answer.
    #[test]
    fn the_same_frames_fold_to_the_same_answer(frames in frames()) {
        let (first_chunks, first) = decode(&frames);
        let (second_chunks, second) = decode(&frames);

        prop_assert_eq!(first, second);
        prop_assert_eq!(first_chunks, second_chunks);
    }

    /// TC-PROP-STREAM-4: the finish reason is the last one stated, or `stop`.
    ///
    /// Input: any frame list, including lists that state a reason more than
    /// once and lists that state none.
    /// Expected: `finish_reason` is the last reason the accepted frames
    /// carried, and `stop` when they carried none. A stream that ended without
    /// saying why is a finished answer, not an answer with an empty reason.
    #[test]
    fn the_finish_reason_is_the_last_one_stated(frames in frames()) {
        let (_, response) = decode(&frames);
        let expected = fold(&frames)
            .finish
            .unwrap_or_else(|| DEFAULT_FINISH_REASON.to_string());

        prop_assert_eq!(response.finish_reason, expected);
    }

    /// TC-PROP-STREAM-5: nothing after the sentinel reaches the answer.
    ///
    /// Input: any frame list, then `[DONE]`, then any second frame list.
    /// Expected: the answer equals the one the first list alone folds to. The
    /// sentinel ends the answer, so a provider that keeps talking cannot
    /// append to a message it already finished.
    #[test]
    fn the_sentinel_ends_the_answer(before in frames(), after in frames()) {
        let mut whole = before.clone();
        whole.push(Frame::Done);
        whole.extend(after);

        let (_, closed) = decode(&whole);
        let (_, alone) = decode(&before);

        prop_assert_eq!(closed, alone);
    }
}

/// TC-PROP-STREAM-6: the decoder is total over arbitrary text.
///
/// Input: any list of arbitrary strings, pushed as frames.
/// Expected: no push and no `finish` panics; every refusal is `PROTOCOL` for
/// text that is not JSON or `PROVIDER` for a frame that carries an error, and
/// never another code. A decoder that panicked on a malformed frame would
/// take the turn down instead of failing it.
#[test]
fn any_text_is_decoded_or_refused() {
    proptest!(|(payloads in prop::collection::vec(any::<String>(), 0..8))| {
        let mut decoder = StreamDecoder::default();
        for payload in &payloads {
            if let Err(error) = decoder.push(payload) {
                prop_assert!(
                    matches!(error.code(), "PROTOCOL" | "PROVIDER"),
                    "unexpected refusal for {payload:?}: {error}",
                );
            }
        }
        let (_, response) = decoder.finish();
        prop_assert!(!response.finish_reason.is_empty(), "an answer always says why it stopped");
    });
}

/// One frame as a case asks for it, before it is rendered onto the wire.
#[derive(Debug, Clone, PartialEq)]
enum Frame {
    Text(String),
    Reasoning(String),
    Call {
        index: u64,
        id: Option<String>,
        name: String,
        arguments: String,
    },
    Finish(String),
    Done,
    Blank,
}

fn frames() -> impl Strategy<Value = Vec<Frame>> {
    let call = (
        0..3u64,
        prop::option::of("[a-z]{1,4}"),
        "[a-z]{0,3}",
        arguments(),
    )
        .prop_map(|(index, id, name, arguments)| Frame::Call {
            index,
            id,
            name,
            arguments,
        });
    let frame = prop_oneof![
        "[a-z ]{0,6}".prop_map(Frame::Text),
        "[a-z ]{0,6}".prop_map(Frame::Reasoning),
        call,
        prop_oneof![Just("stop"), Just("tool_calls"), Just("length")]
            .prop_map(|reason| Frame::Finish(reason.to_string())),
        Just(Frame::Done),
        Just(Frame::Blank),
    ];
    prop::collection::vec(frame, 0..10)
}

/// Argument fragments, so a generated call sometimes builds JSON, sometimes
/// builds text that is no JSON, and sometimes builds nothing.
fn arguments() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("{\"a\":".to_string()),
        Just("1}".to_string()),
        Just("{}".to_string()),
        Just("not json".to_string()),
    ]
}

/// The wire text of one frame.
fn wire(frame: &Frame) -> String {
    match frame {
        Frame::Text(delta) => {
            serde_json::json!({ "choices": [{ "delta": { "content": delta } }] }).to_string()
        }
        Frame::Reasoning(delta) => {
            serde_json::json!({ "choices": [{ "delta": { "reasoning_content": delta } }] })
                .to_string()
        }
        Frame::Call {
            index,
            id,
            name,
            arguments,
        } => {
            let mut call = serde_json::json!({
                "index": index,
                "function": { "name": name, "arguments": arguments },
            });
            if let Some(id) = id {
                call["id"] = serde_json::json!(id);
            }
            serde_json::json!({ "choices": [{ "delta": { "tool_calls": [call] } }] }).to_string()
        }
        Frame::Finish(reason) => {
            serde_json::json!({ "choices": [{ "finish_reason": reason, "delta": {} }] }).to_string()
        }
        Frame::Done => "[DONE]".to_string(),
        Frame::Blank => String::new(),
    }
}

/// Push every frame and close the stream, keeping only what `finish` emits.
fn decode(frames: &[Frame]) -> (Vec<StreamChunk>, ModelResponse) {
    let mut decoder = StreamDecoder::default();
    for frame in frames {
        decoder
            .push(&wire(frame))
            .expect("a rendered frame is JSON");
    }
    decoder.finish()
}

/// The same, keeping what the pushes emitted rather than what `finish` did.
fn pushed(frames: &[Frame]) -> (Vec<StreamChunk>, ModelResponse) {
    let mut decoder = StreamDecoder::default();
    let mut streamed = Vec::new();
    for frame in frames {
        streamed.extend(
            decoder
                .push(&wire(frame))
                .expect("a rendered frame is JSON"),
        );
    }
    let (_, response) = decoder.finish();
    (streamed, response)
}

/// What the frames say, folded a second time by the case itself.
#[derive(Default)]
struct Expected {
    content: String,
    reasoning: String,
    calls: BTreeMap<u64, (String, String, String)>,
    finish: Option<String>,
}

impl Expected {
    /// The calls as the decoder must answer with them: an unnamed id becomes
    /// `call_<index>`, and arguments that are no JSON stay as text.
    fn tool_calls(&self) -> Vec<ToolCall> {
        self.calls
            .iter()
            .map(|(index, (id, name, arguments))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{index}")
                } else {
                    id.clone()
                },
                name: name.clone(),
                arguments: if arguments.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()))
                },
            })
            .collect()
    }
}

fn fold(frames: &[Frame]) -> Expected {
    let mut expected = Expected::default();
    for frame in frames {
        match frame {
            Frame::Done => break,
            Frame::Blank => {}
            Frame::Text(delta) => expected.content.push_str(delta),
            Frame::Reasoning(delta) => expected.reasoning.push_str(delta),
            Frame::Finish(reason) => expected.finish = Some(reason.clone()),
            Frame::Call {
                index,
                id,
                name,
                arguments,
            } => {
                let partial = expected.calls.entry(*index).or_default();
                if let Some(id) = id {
                    partial.0 = id.clone();
                }
                partial.1.push_str(name);
                partial.2.push_str(arguments);
            }
        }
    }
    expected
}
