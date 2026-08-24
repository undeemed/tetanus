//! Shrinking a tool result that is too long to keep whole, deterministically
//! and without asking a model.
//!
//! A coding agent's tool results are the part of a session that grows without
//! limit: one `cat` of a large file can cost more context than the whole
//! conversation around it. Pruning keeps the head and the tail of an
//! over-long result and says, in the middle, that something was removed.
//!
//! **Head and tail, not a summary.** The two ends are where the information
//! is: the head says what the command was doing and the tail says how it
//! ended. A summary would need a model, which would make the transform
//! non-deterministic, unreplayable and billable - so this does not have one.
//!
//! **It is a pure function of its input**, which is what keeps a replayed
//! session honest. Deriving history from a journal must produce the same
//! request every time, so anything that shapes that history has to be
//! reproducible from the journal alone. A clock, a model or a random choice
//! here would break the rule `upstream_request_reconstruction.rs` pins.
//!
//! **Measurement is in characters, never bytes.** Slicing a UTF-8 string at a
//! byte offset panics in the middle of a character, and the offsets here come
//! from a budget rather than from the text, so they land mid-character
//! routinely. Upstream states the same rule as "without splitting surrogate
//! pairs"; the hazard is the encoding's, and every encoding has one.
//!
//! Parity: upstream `packages/compaction/compaction-tool-result-pruner`, the
//! content-transform half, pinned by its `tool-result-pruner.spec.ts`. The
//! session transaction it also performs - rewriting the log with a shadow node
//! that cites the result it replaced, and pricing it - needs a durable event
//! type this contract has not published, so it stays phase (2).

/// What stands in for the span a prune removed.
///
/// It is text the model reads, so it says what happened rather than being a
/// silent gap: a result that just stopped in the middle would look like a
/// tool that failed halfway.
pub const MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

/// The budgets a prune runs under, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneBudget {
    /// Results at or below this length are left exactly as they are.
    pub threshold: usize,
    /// How much of the start to keep.
    pub head: usize,
    /// How much of the end to keep.
    pub tail: usize,
}

impl Default for PruneBudget {
    /// Upstream's own figures, which are tuned for coding-agent tool output:
    /// enough head to see what a command was doing and enough tail to see how
    /// it ended.
    fn default() -> Self {
        Self {
            threshold: 8192,
            head: 4096,
            tail: 1024,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PruneError {
    #[error("threshold must be at least 1")]
    EmptyThreshold,
    /// The budget would make a long result *longer*. A prune that grew its
    /// input would loop forever against a caller that prunes until it fits,
    /// which is why this is refused where the budget is set rather than
    /// noticed where it is applied.
    #[error(
        "head ({head}) + marker ({marker}) + tail ({tail}) is {total}, which must be at most \
         threshold ({threshold}): a prune that emitted more than it accepts would never converge"
    )]
    WouldNotShrink {
        head: usize,
        marker: usize,
        tail: usize,
        total: usize,
        threshold: usize,
    },
}

impl PruneBudget {
    /// Check that these budgets can actually shrink something.
    pub fn validate(self) -> Result<Self, PruneError> {
        if self.threshold == 0 {
            return Err(PruneError::EmptyThreshold);
        }
        let marker = length(MARKER);
        let total = self.head + marker + self.tail;
        if total > self.threshold {
            return Err(PruneError::WouldNotShrink {
                head: self.head,
                marker,
                tail: self.tail,
                total,
                threshold: self.threshold,
            });
        }
        Ok(self)
    }
}

/// How long a result is, for this transform's purposes.
///
/// Characters, because that is what the budgets are in and what a slice must
/// respect. This is deliberately not a token count: a token measure would tie
/// pruning to a particular provider's tokenizer, and the point of this
/// transform is that it is the same everywhere and needs nothing to run.
pub fn length(text: &str) -> usize {
    text.chars().count()
}

/// Shrink `text` if it is over budget, or answer `None` if it is not.
///
/// `None` rather than the unchanged text on purpose: a caller usually wants to
/// know whether anything happened - to record it, or to stop pruning - and an
/// answer it has to compare against the input to find out is an answer that
/// invites the comparison being skipped.
pub fn prune(text: &str, budget: PruneBudget) -> Option<String> {
    if length(text) <= budget.threshold {
        return None;
    }

    // Collected once: `chars()` is a linear walk, and taking a head and a tail
    // from the same string otherwise walks it twice for no reason.
    let characters: Vec<char> = text.chars().collect();
    let head: String = characters.iter().take(budget.head).collect();
    let tail: String = characters
        .iter()
        .skip(characters.len().saturating_sub(budget.tail))
        .collect();

    Some(format!("{head}{MARKER}{tail}"))
}

/// Prune if over budget, else hand back what was given.
///
/// The convenience form, for a caller that only wants the text and has no use
/// for knowing whether it changed.
pub fn pruned(text: &str, budget: PruneBudget) -> String {
    prune(text, budget).unwrap_or_else(|| text.to_string())
}
