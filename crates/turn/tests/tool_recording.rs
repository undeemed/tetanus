//! Test Design Specification: what the journal keeps of a call's arguments.
//!
//! Feature under test: [`Tool::recorded`] and the registry method the engine
//! calls before it appends. A tool call is written down before it runs, so
//! what a tool is *given* and what is *kept* are two different things - and
//! for an argument that carries a credential they have to be.
//!
//! Why this exists: `terminal_send` is how a model answers `[sudo] password
//! for ci:`, and the presentation lane found the password in
//! `sessions/<id>.jsonl` in plain text, permanently, drawn by every surface
//! that draws a tool call
//! (`docs/contract-updates/ui-terminal-send-secrets.md`). Redacting on screen
//! would have hidden the risk rather than removed it, so the decision is made
//! where the record is written.
//!
//! Approach: the registry alone. What `crates/exec`'s tools do with the seam
//! is asserted where those tools are, driven through a real turn; this pins
//! the seam's own contract, including the two directions it can fail in.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::json;
use tetanus_turn::tools::{
    Tool, ToolCall, ToolError, ToolOutcome, ToolRegistry, ToolSchema, REDACTED,
};

/// TC-TOOL-RECORD-1: a tool that says nothing has everything recorded.
///
/// The default direction is the opposite of this file's siblings, and it is
/// deliberate: an argument nobody can read is an audit trail nobody can
/// follow. Withholding is for the arguments a tool knows can carry a secret,
/// and a tool written before this seam existed keeps behaving as it did.
///
/// Input: a tool that does not override `recorded`.
/// Expected: the arguments exactly as the model sent them.
#[test]
fn a_tool_that_says_nothing_has_everything_recorded() {
    let registry = ToolRegistry::new().with(Arc::new(Plain));

    assert_eq!(
        registry.recorded(&call("plain", json!({ "text": "nothing secret here" }))),
        json!({ "text": "nothing secret here" })
    );
}

/// TC-TOOL-RECORD-2: a tool withholds one argument and keeps the rest.
///
/// The shape the fix needs. Replacing the whole argument object would destroy
/// what an audit is for: the record still has to say which tool was called,
/// with which session, at which moment - only the credential goes.
///
/// Input: a tool that withholds `text` when the call says it is secret.
/// Expected: `text` is the sentinel, every other argument is untouched, and
/// the same call with no flag is recorded whole.
#[test]
fn a_tool_withholds_one_argument_and_keeps_the_rest() {
    let registry = ToolRegistry::new().with(Arc::new(Careful));

    let withheld = registry.recorded(&call(
        "careful",
        json!({ "session_id": "t-1", "text": "hunter2", "secret": true }),
    ));
    assert_eq!(withheld["text"], json!(REDACTED));
    assert_eq!(withheld["session_id"], json!("t-1"));
    assert_eq!(withheld["secret"], json!(true));

    let kept = registry.recorded(&call(
        "careful",
        json!({ "session_id": "t-1", "text": "ls -al" }),
    ));
    assert_eq!(kept["text"], json!("ls -al"));
}

/// TC-TOOL-RECORD-3: a redactor that panics withholds everything.
///
/// The fail-closed direction, and it points the opposite way from
/// [`ToolMode`]'s: a panicking classifier costs concurrency, a panicking
/// redactor costs a credential. The tool was the only thing that knew which
/// argument was a secret, so a panic loses that knowledge - and the safe
/// answer is to keep none of them rather than to write them all down at the
/// moment the protection broke.
///
/// Input: a tool whose `recorded` panics.
/// Expected: the sentinel in place of the whole argument object, and the
/// registry still answers rather than unwinding into the engine.
#[test]
fn a_redactor_that_panics_withholds_everything() {
    let registry = ToolRegistry::new().with(Arc::new(Broken));

    assert_eq!(
        registry.recorded(&call("broken", json!({ "text": "hunter2" }))),
        json!(REDACTED)
    );
}

/// TC-TOOL-RECORD-4: a call naming no tool is recorded as it was sent.
///
/// It is about to fail as unknown, and the arguments are the evidence: a model
/// that called `bash` on a build that offers `shell` is diagnosed from what it
/// actually wrote. Nothing claimed these arguments, so nothing withheld them.
///
/// Input: a call naming a tool the registry does not have.
/// Expected: the arguments unchanged.
#[test]
fn a_call_naming_no_tool_is_recorded_as_it_was_sent() {
    let registry = ToolRegistry::new().with(Arc::new(Plain));

    assert_eq!(
        registry.recorded(&call("no-such-tool", json!({ "text": "what did it send" }))),
        json!({ "text": "what did it send" })
    );
}

/// TC-TOOL-RECORD-5: what counts as a credential prompt.
///
/// The rule behind the backstop, pinned on its own because it is the half that
/// can be wrong in two directions and the expensive direction is silent. A
/// miss leaks a password; a false positive costs the auditability of one
/// command, which is why the rule is allowed to be generous but not arbitrary.
///
/// It is `sudo`'s mechanism - `sudo` had full access to the terminal's `ECHO`
/// flag and chose a regex over the program's output instead - narrowed to the
/// last non-empty line, because a prompt is by definition the last thing
/// written before a program waits, and matching anywhere makes a `grep` hit
/// for the word arm the filter.
///
/// Input: the prompts `sudo`, `ssh`, `su` and `passwd` actually print; then
/// output that merely mentions a password, and output that mentioned one
/// several lines ago.
/// Expected: every real prompt matches; neither of the others does.
#[test]
fn what_counts_as_a_credential_prompt() {
    use tetanus_turn::tools::looks_like_a_password_prompt as prompt;

    for real in [
        "[sudo] password for ci:",
        "[sudo] password for ci: ",
        "Password:",
        "password: ",
        "Enter passphrase for key '/root/.ssh/id_rsa':",
        "ci@10.0.0.4's password:",
        "Enter new UNIX password:",
        "Repeat password?",
        "some output first\n[sudo] password for ci: ",
    ] {
        assert!(prompt(real), "should be read as a prompt: {real:?}");
    }

    for not in [
        "",
        "\n\n",
        "the password was wrong, try again with -v",
        "grep: config.yml: password: hunter2",
        "[sudo] password for ci:\nauthentication failed\n$ ",
        "Password saved to the keychain, you are logged in",
    ] {
        assert!(!prompt(not), "should not be read as a prompt: {not:?}");
    }
}

// ---------------------------------------------------------------- fixtures

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments,
    }
}

fn schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: String::new(),
        parameters: json!({ "type": "object" }),
    }
}

/// A tool with nothing to hide.
struct Plain;

#[async_trait::async_trait]
impl Tool for Plain {
    fn schema(&self) -> ToolSchema {
        schema("plain")
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("ran"))
    }
}

/// A tool that can be told one of its arguments is a credential.
struct Careful;

#[async_trait::async_trait]
impl Tool for Careful {
    fn schema(&self) -> ToolSchema {
        schema("careful")
    }

    fn recorded(&self, arguments: &serde_json::Value) -> serde_json::Value {
        if !arguments
            .get("secret")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return arguments.clone();
        }
        let mut recorded = arguments.clone();
        if let Some(object) = recorded.as_object_mut() {
            object.insert("text".into(), json!(REDACTED));
        }
        recorded
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("ran"))
    }
}

/// A tool whose redactor has a bug in it.
struct Broken;

#[async_trait::async_trait]
impl Tool for Broken {
    fn schema(&self) -> ToolSchema {
        schema("broken")
    }

    fn recorded(&self, _arguments: &serde_json::Value) -> serde_json::Value {
        panic!("this redactor has a bug in it");
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("ran"))
    }
}
