//! How deep delegation may go.
//!
//! An agent can start a child agent, and that child can start its own. Without
//! a budget that recursion has no floor: a persona that always delegates would
//! spawn agents until the machine stopped, each one spending real money on
//! real model calls. Delegation depth is that budget, counted from zero at the
//! top and incremented for each generation.
//!
//! # The rule that carries the safety: depth is monotone
//!
//! A depth arrives from two places — the session header, which was persisted
//! when the child was created, and the runtime options of the agent as it is
//! running now. The effective depth is the **larger** of the two.
//!
//! That is not an arbitrary tie-break. A resumed child is constructed with
//! fresh options, and if the runtime value were simply believed, a child
//! resumed with no options would count itself as top-level and delegate as
//! though it had a full budget. Taking the maximum means runtime may *deepen*
//! the count and can never shorten it, so a resume cannot buy back depth.
//!
//! # Where validation lives
//!
//! Upstream validates depths at every use because a JavaScript number can be
//! `-0`, `NaN`, or `1.5`. Here the in-memory type is [`u64`], so none of those
//! are representable and the checks would be unreachable. The validation that
//! remains is at the boundary where those values can still arrive: reading a
//! depth out of JSON, in [`depth_from_json`].
//!
//! Parity: upstream `packages/subagent/subagent/src/depth.ts`, and the depth
//! rules of its `service.spec.ts` and `continuation.spec.ts`.

use serde_json::Value;

/// A depth or cap that was written down but is not a usable one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DepthError {
    /// The value was not a whole, non-negative number.
    #[error("{field} must be a non-negative safe integer")]
    NotAWholeCount {
        /// Which field was wrong, so a reader can find it.
        field: &'static str,
    },
    /// A child would be deeper than its cap allows.
    #[error("subagent depth {attempted} exceeds maxDepth {max}")]
    TooDeep {
        /// The depth the child would have had.
        attempted: u64,
        /// The cap that refused it.
        max: u64,
    },
}

/// The depth an agent actually has.
///
/// The maximum of what was persisted and what the runtime claims, so runtime
/// can deepen the count but never shorten it. See the module note: a resumed
/// child that counted itself from zero would delegate as if it were top-level.
pub fn delegation_depth_of(header_depth: Option<u64>, runtime_depth: Option<u64>) -> u64 {
    header_depth.unwrap_or(0).max(runtime_depth.unwrap_or(0))
}

/// The depth a child of this agent would have.
pub fn child_depth(parent_depth: u64) -> u64 {
    parent_depth.saturating_add(1)
}

/// Whether a child at `attempted` is allowed under `max`.
///
/// No cap means no limit. A cap of zero forbids delegation entirely, which is
/// a configuration a deployment may reasonably want and so is not special.
pub fn check_within_max(attempted: u64, max: Option<u64>) -> Result<(), DepthError> {
    match max {
        Some(max) if attempted > max => Err(DepthError::TooDeep { attempted, max }),
        _ => Ok(()),
    }
}

/// Read a depth or cap out of a configuration document.
///
/// This is the boundary the unrepresentable values arrive through, so it is
/// the only place they are checked. Absent is `None` and means "not set",
/// which is different from zero: a cap of zero forbids delegation, and an
/// unset cap does not.
pub fn depth_from_json(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<u64>, DepthError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    // `as_u64` already rejects a negative or fractional number, and every
    // non-number. It is checked through the float too, so that a value written
    // as `2.0` - which JSON does not distinguish from `2` - is not refused for
    // a spelling.
    if let Some(whole) = value.as_u64() {
        return Ok(Some(whole));
    }
    if let Some(number) = value.as_f64() {
        if number.is_finite() && number >= 0.0 && number.fract() == 0.0 {
            // Bounded by the check above, so the cast cannot wrap.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            return Ok(Some(number as u64));
        }
    }
    Err(DepthError::NotAWholeCount { field })
}
