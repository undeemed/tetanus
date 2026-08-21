//! The names a binding namespace may not have, and why every backend refuses
//! the same ones.
//!
//! **One shared set, not one per backend.** A namespace list that works
//! against the local runtime has to work against the remote one, or the
//! portability the seam promises is a promise about whichever backend the
//! deployment happened to test. So a name is refused everywhere as soon as it
//! is refused anywhere - which is upstream's rule, and the reason its set
//! names Python slots that the TypeScript backend could have accepted.
//!
//! tetanus keeps the union rather than narrowing it to the language its own
//! local backend evaluates. Narrowing would be the same mistake one level
//! down: a binding called `lambda` would work here and fail the day a Python
//! backend lands, and the day it lands is not the day to find out.

/// The identifier shape every target language shares: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// Written out rather than matched with a regular expression, because the
/// workspace has no regex dependency and this rule is four lines.
pub fn is_portable_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Globals every backend refuses because *some* backend owns the slot.
///
/// `console` is the log capture; the rest are a Python bootstrap's own
/// globals. They are all valid portable identifiers, which is exactly why the
/// list has to exist: the identifier rule would let them through.
pub const RESERVED_BINDING_GLOBALS: &[&str] = &[
    "console",
    "__dsh_main__",
    "__builtins__",
    "__name__",
    "__debug__",
];

/// Reserved words of every portable target language: ECMAScript's union
/// Python's.
///
/// A per-language check would let `lambda` pass here and fail a Python
/// backend, so the union is checked everywhere.
pub const PORTABLE_RESERVED_WORDS: &[&str] = &[
    // ECMAScript reserved words and the names reserved in strict mode.
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "let",
    "static",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "arguments",
    "eval",
    // Python keywords and soft keywords not already above. `type` and `_` are
    // soft keywords - legal names in practice - and are reserved anyway,
    // because a binding named `_` is not worth the ambiguity.
    "False",
    "None",
    "True",
    "and",
    "as",
    "assert",
    "async",
    "def",
    "del",
    "elif",
    "except",
    "from",
    "global",
    "is",
    "lambda",
    "nonlocal",
    "not",
    "or",
    "pass",
    "raise",
    "match",
    "type",
    "_",
];

/// Property names a typed error class may not use: JavaScript's `Error` fields
/// and Python's exception protocol.
///
/// This crate has no typed-rejection contract yet - a binding failure is a
/// message the program sees - so nothing calls [`check_error_member`] on a
/// live path. It is kept, and tested, because the set is the portable half of
/// the seam: the day a backend materializes a typed rejection, the names it
/// may not use are already settled, and `docs/parity.md` records that the rest
/// of that contract is unported rather than lost.
pub const RESERVED_ERROR_MEMBERS: &[&str] = &[
    "name",
    "message",
    "stack",
    "args",
    "with_traceback",
    "add_note",
];

/// Whether a name is dunder-form: `__x__` with a non-empty middle.
///
/// Refused wholesale as an error member, because several are CPython
/// descriptors whose assignment raises while the rejection is being built, and
/// which ones is an interpreter version detail nobody should encode.
pub fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

/// Whether a namespace may be exposed under this global, and why not when it
/// may not.
pub fn check_global(global: &str) -> Result<(), String> {
    if global.is_empty() {
        return Err("a namespace needs a name".to_string());
    }
    if !is_portable_identifier(global) {
        return Err(format!(
            "a namespace global is a portable identifier ([A-Za-z_][A-Za-z0-9_]*), and {global:?} \
             is not one on every target language"
        ));
    }
    if RESERVED_BINDING_GLOBALS.contains(&global) {
        return Err(format!("{global:?} is a slot a backend owns"));
    }
    if PORTABLE_RESERVED_WORDS.contains(&global) {
        return Err(format!(
            "{global:?} is a reserved word in a language this seam promises to be portable to"
        ));
    }
    Ok(())
}

/// Whether a typed error class may carry this member.
pub fn check_error_member(member: &str) -> Result<(), String> {
    if member.is_empty() {
        return Err("an error member needs a name".to_string());
    }
    if is_dunder(member) {
        return Err(format!(
            "{member:?} is dunder-form, which is an object-protocol slot in Python"
        ));
    }
    if RESERVED_ERROR_MEMBERS.contains(&member) {
        return Err(format!(
            "{member:?} belongs to the error protocol of a target language"
        ));
    }
    Ok(())
}
