//! Which settings keys hold a credential.
//!
//! A document may hold a provider's key beside the settings that provider
//! reads, so a surface that publishes resolved configuration has to know which
//! values it must not publish. Upstream decides this from a schema role
//! (`redactSecrets`); tetanus has no schema for a key it does not settle, so
//! the name is what it has to go on.

/// The words a key ends in when it holds a credential.
///
/// A closed list, and short on purpose: a word added here hides a value a user
/// wrote and expects to read back.
const WORDS: [&str; 5] = ["key", "secret", "token", "password", "credential"];

/// Whether `key` names a value that must not be published.
///
/// The last word of the key decides: `llm.providers.deepseek.api_key` holds a
/// credential, while `llm.providers.deepseek.api_key_env` names the
/// environment variable that holds one and `agent.max_tokens` is a budget. A
/// word and not a substring, so `monkey` is a setting like any other; and the
/// last word only, so a section named `credentials` does not hide the settings
/// under it.
pub fn names_a_secret(key: &str) -> bool {
    words(key)
        .last()
        .is_some_and(|word| WORDS.contains(&word.as_str()))
}

/// A key as the words a reader sees in it, lowercased. `.`, `_` and `-`
/// separate words, and so does a capital that starts one - both the `Key` of
/// `apiKey` and the `Key` of `APIKey`.
fn words(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, &c) in chars.iter().enumerate() {
        if c == '.' || c == '_' || c == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        // A capital starts a word when it follows a lowercase letter, and when
        // it is the last capital of a run that a lowercase letter follows.
        let starts_word = c.is_uppercase()
            && (chars
                .get(index.wrapping_sub(1))
                .is_some_and(|previous| previous.is_lowercase() || previous.is_numeric())
                || (index > 0 && chars.get(index + 1).is_some_and(|next| next.is_lowercase())));
        if starts_word && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}
