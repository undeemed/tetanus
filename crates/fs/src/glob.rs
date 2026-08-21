//! The pattern language `glob` accepts, and the matcher that reads it.
//!
//! **A closed, small language, not a regular expression.** Four things mean
//! something - `**` for any run of directories, `*` for any run of characters
//! inside one name, `?` for one character, and everything else literally - and
//! nothing else does. A model that writes a regular expression by mistake gets
//! no matches rather than a surprising set of them, and a pattern cannot make
//! the matcher expensive: matching is over the pattern's own segments, so its
//! cost is bounded by what the caller wrote, not by what is on the disk.
//!
//! **Hidden entries are opt in.** A walk that descended into `.git` would spend
//! its whole budget there and answer with objects nobody asked for. A pattern
//! that names a dot segment - `.github/**`, `.*rc` - is asking for them and
//! gets them; one that does not, does not. This is a convention rather than a
//! rule of any glob standard, and it is the convention every tool a coding
//! agent replaces already follows.
//!
//! Parity: upstream's `glob` tool is `packages/fs/tool-fs-search`, which shells
//! out to `ripgrep` and `fd`. tetanus answers the same question in-process: a
//! harness that needs an external binary to list files is a harness that fails
//! differently on every machine, and `docs/parity-updates/` records the split.

use crate::error::FsError;

/// One segment of a parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// `**`: zero or more whole path segments.
    AnyDepth,
    /// Anything else: a name pattern matched against one path segment.
    Name(Vec<Token>),
}

/// One piece of a name pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A run of literal characters.
    Literal(String),
    /// `*`: any run of characters, including none, never crossing a separator.
    Any,
    /// `?`: exactly one character.
    One,
}

/// A parsed glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    segments: Vec<Segment>,
    hidden: bool,
}

impl Pattern {
    /// Read a pattern, or say why it cannot be read.
    ///
    /// The two refusals are both about a pattern that could not match what the
    /// caller meant: an empty one names nothing, and an absolute one is asking
    /// for a path outside the directory the search starts from - which the
    /// fence would refuse anyway, so saying it here names the real mistake.
    pub fn parse(pattern: &str) -> Result<Self, FsError> {
        if pattern.trim().is_empty() {
            return Err(FsError::BadPattern {
                pattern: pattern.to_string(),
                reason: "a glob must name something".into(),
            });
        }
        if pattern.starts_with('/') {
            return Err(FsError::BadPattern {
                pattern: pattern.to_string(),
                reason: "a glob is matched relative to the directory the search starts from, so \
                         it cannot begin with \"/\""
                    .into(),
            });
        }

        let mut segments = Vec::new();
        let mut hidden = false;
        for raw in pattern.split('/') {
            match raw {
                // A repeated or empty segment is noise a caller did not mean:
                // `a//b` and `a/./b` both name `a/b`.
                "" | "." => continue,
                ".." => {
                    return Err(FsError::BadPattern {
                        pattern: pattern.to_string(),
                        reason: "a glob cannot walk upwards with \"..\"; start the search from \
                                 the directory you mean"
                            .into(),
                    })
                }
                "**" => segments.push(Segment::AnyDepth),
                name => {
                    hidden |= name.starts_with('.');
                    segments.push(Segment::Name(tokens(name)));
                }
            }
        }
        if segments.is_empty() {
            return Err(FsError::BadPattern {
                pattern: pattern.to_string(),
                reason: "a glob must name something".into(),
            });
        }
        Ok(Self { segments, hidden })
    }

    /// Whether this pattern asked for dot-prefixed entries.
    pub fn wants_hidden(&self) -> bool {
        self.hidden
    }

    /// Whether a path, split into its segments, matches.
    pub fn matches(&self, parts: &[String]) -> bool {
        walk(&self.segments, parts)
    }
}

/// Split a name pattern into its tokens, folding runs of literal characters
/// together so the matcher compares strings rather than characters.
fn tokens(name: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut literal = String::new();
    for ch in name.chars() {
        match ch {
            '*' | '?' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(if ch == '*' { Token::Any } else { Token::One });
            }
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    tokens
}

/// Match segments against path parts, backtracking over `**`.
///
/// Recursion depth is bounded by the pattern, not by the path: each call
/// consumes a segment, and the `AnyDepth` arm is the only one that does not -
/// it consumes path parts instead, so the pair still terminates.
fn walk(segments: &[Segment], parts: &[String]) -> bool {
    match segments.split_first() {
        None => parts.is_empty(),
        Some((Segment::AnyDepth, rest)) => {
            // `**` at the end matches everything below, including nothing.
            if rest.is_empty() {
                return true;
            }
            (0..=parts.len()).any(|skip| walk(rest, &parts[skip..]))
        }
        Some((Segment::Name(name), rest)) => match parts.split_first() {
            Some((part, tail)) => name_matches(name, part) && walk(rest, tail),
            None => false,
        },
    }
}

/// Whether one name pattern matches one path segment.
fn name_matches(tokens: &[Token], name: &str) -> bool {
    match tokens.split_first() {
        None => name.is_empty(),
        Some((Token::Literal(text), rest)) => match name.strip_prefix(text.as_str()) {
            Some(tail) => name_matches(rest, tail),
            None => false,
        },
        Some((Token::One, rest)) => match name.chars().next() {
            Some(ch) => name_matches(rest, &name[ch.len_utf8()..]),
            None => false,
        },
        Some((Token::Any, rest)) => {
            // Every split of the remaining name, shortest first. `rest` is
            // shorter at each step, so this terminates however many `*`s the
            // pattern holds.
            if rest.is_empty() {
                return true;
            }
            std::iter::once(0)
                .chain(name.char_indices().map(|(index, ch)| index + ch.len_utf8()))
                .any(|split| name_matches(rest, &name[split..]))
        }
    }
}
