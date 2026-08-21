//! Permission presets: the two independent knobs, named as one choice a
//! person can make.
//!
//! A deployment has two dials - how much of the filesystem a session may touch
//! ([`FsMode`]) and what happens when a tool asks whether it may run
//! ([`ApprovalPolicy`]) - and nobody thinks in dials. They think "let it work
//! in this directory" or "read only, I am watching". A preset is that sentence,
//! and it is the whole of what this module adds: it invents no third mechanism.
//!
//! **A switch records the intent, then writes the knobs.** `permission/preset`
//! says which preset a person picked; `approval/policy` and `fs/mode` are what
//! actually decide anything, and every reader keeps reading its own knob. Two
//! presets can bundle the same pair, so without the intent event a journal
//! could not say which was chosen - and the answer a surface shows back should
//! be the words the person used.
//!
//! **The fold is the whole state**, exactly as it is for the approval policy
//! (contract section 4.4.7): the last event on the journal wins, and a resumed
//! session is under what it was under with nothing to replay but the log.
//!
//! **Where tetanus parts from upstream, and why.** Upstream's
//! `danger-full-access` preset bundles the `never` approval policy, because
//! there `never` means "do not prompt" and its prompts are escalation requests
//! that full access has already made unnecessary. In tetanus `never` means
//! every ask settles `rejected` - that is contract section 4.4.7, and it is
//! what makes an unattended run neither hang nor depend on a client. Bundling
//! it with full access would therefore make the widest preset refuse the very
//! calls the narrower one allows. So the widest preset here pairs full access
//! with `ask`, and a deployment that wants irreversible calls to run unattended
//! attaches an answerer that grants them: a decision with a name and a code
//! path, rather than a word in a table whose effect is the opposite of what it
//! reads like.
//!
//! Parity: upstream `packages/interaction/permission-presets`, pinned by its
//! `permission-presets.spec.ts` and `projection.spec.ts`.

use serde_json::json;
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::approval::{set_policy, ApprovalPolicy};
use tetanus_turn::log::topic as turn_topic;

use crate::access::FsMode;

/// The durable vocabulary this module writes.
pub mod topic {
    /// The preset a person chose, as intent. Log-only: nothing executes on it,
    /// and it never reaches the model.
    pub const PERMISSION_PRESET: &str = "permission/preset";
    /// The filesystem mode knob. The last one on the journal is the session's,
    /// exactly as `approval/policy` works.
    pub const FS_MODE: &str = "fs/mode";
}

/// One preset: a name, the two knob values it stands for, and a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub mode: FsMode,
    pub approval: ApprovalPolicy,
    /// One user-facing sentence saying what the preset means. A surface shows
    /// it beside the name; a person choosing between three words needs it.
    pub description: &'static str,
}

/// The table a deployment gets without configuring one.
///
/// Three, in widening order, so a surface can render them as a scale. Adding a
/// fourth is a deliberate change here rather than a value a document can
/// invent, because a preset that names knob values nothing checks is a
/// misconfiguration a session only discovers when a tool is refused.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "read-only",
        mode: FsMode::ReadOnly,
        approval: ApprovalPolicy::Never,
        description: "Read the workspace and change nothing. Every mutation is refused, and \
                      nothing is put to you to decide.",
    },
    Preset {
        name: "workspace-write",
        mode: FsMode::WorkspaceWrite,
        approval: ApprovalPolicy::Ask,
        description: "Work inside the workspace. Anything a session cannot take back is put to \
                      you first.",
    },
    Preset {
        name: "danger-full-access",
        mode: FsMode::DangerFullAccess,
        approval: ApprovalPolicy::Ask,
        description: "No filesystem fence at all. Anything a session cannot take back is still \
                      put to you first.",
    },
];

/// The preset a deployment that names none is under.
pub const DEFAULT_PRESET: &str = "workspace-write";

/// Look one up by name.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.name == name)
}

#[derive(Debug, thiserror::Error)]
#[error("no permission preset is named {name:?}; the presets are {}", listed())]
pub struct UnknownPreset {
    pub name: String,
}

fn listed() -> String {
    PRESETS
        .iter()
        .map(|preset| preset.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The preset a session was last switched to, or `None` when it never was.
///
/// The name only. What a preset *does* is the two knobs, and they are folded
/// separately, so a journal written by a build with a different table still
/// executes correctly here - it just reports a preset name this build cannot
/// describe.
pub fn effective_preset(events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::PERMISSION_PRESET)
        .and_then(|event| event.data["preset"].as_str())
        .map(str::to_string)
}

/// The filesystem mode a session is under, folded from its journal.
///
/// `None` means it never switched, and the deployment's default stands - the
/// same shape [`tetanus_turn::approval::effective_policy`] has, for the same
/// reason.
pub fn effective_mode(events: &[SessionEvent]) -> Option<FsMode> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::FS_MODE)
        .and_then(|event| event.data["mode"].as_str())
        .and_then(|word| FsMode::parse(word).ok())
}

/// Switch a session to a named preset.
///
/// The order is the contract's and it is not arbitrary: the intent is written
/// first, then each knob that actually changes. A reader replaying the journal
/// therefore sees the choice before its consequences, and a knob already at the
/// value the preset wants is not rewritten - so a switch between two presets
/// that share a knob leaves one event, not two.
///
/// Answers the preset that was applied.
pub fn switch(log: &dyn SessionLog, name: &str) -> Result<&'static Preset, SwitchError> {
    let preset = preset(name).ok_or_else(|| {
        SwitchError::Unknown(UnknownPreset {
            name: name.to_string(),
        })
    })?;
    let events = log.events();

    log.append(topic::PERMISSION_PRESET, json!({ "preset": preset.name }))?;
    if effective_mode(&events) != Some(preset.mode) {
        log.append(topic::FS_MODE, json!({ "mode": preset.mode.as_str() }))?;
    }
    if tetanus_turn::approval::effective_policy(&events) != Some(preset.approval) {
        set_policy(log, preset.approval)?;
    }
    Ok(preset)
}

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error(transparent)]
    Unknown(#[from] UnknownPreset),
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// Whether a durable event is one of the three a permission switch writes.
///
/// Published because a reader that renders a transcript needs to know these
/// carry no model-visible content: like `approval/*`, none of them derives to a
/// message, and what the model learns is the `tool/result` it gets.
pub fn is_permission_event(event: &SessionEvent) -> bool {
    matches!(
        event.ty.as_str(),
        topic::PERMISSION_PRESET | topic::FS_MODE | turn_topic::APPROVAL_POLICY
    )
}
