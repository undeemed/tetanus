//! Test Design Specification: the environment layer.
//!
//! Feature under test: `tetanus_config::env` - turning environment variables
//! into a config layer, and where that layer sits in the stack.
//!
//! There is no upstream suite to port. Upstream reads settings from a document
//! and a service, not from the environment, so these cases are written against
//! the hole in tetanus's own documented stack: `Layer::Env` has existed since
//! the layered config did, the CLI renders `env` as a provenance, and nothing
//! ever put a key there.
//!
//! Approach: every case drives `from_vars`, which takes the variables as an
//! argument. Reading the real environment would mutate state shared by every
//! other case in the binary and make the suite order-dependent, so the rule is
//! tested where it can be stated exactly and `from_env` is the one line that
//! reaches for `std::env`.
//!
//! Environmental needs: none. No case reads or writes the real environment, a
//! filesystem, or a network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_config::env::{from_vars, PREFIX, SEPARATOR};
use tetanus_config::{Config, Layer};

/// TC-ENV-1: a prefixed variable names the key its segments spell.
///
/// Input: variables for a two-segment key, a deep key, and a key whose last
/// segment is itself several words.
/// Expected: the dotted keys the engine actually settles. The multi-word case
/// is the one that decides the separator: `agent.max_parallel_tool_calls` and
/// `agent.max.parallel.tool.calls` would be the same variable under a single
/// underscore.
#[test]
fn a_prefixed_variable_names_the_key_its_segments_spell() {
    let document = from_vars([
        ("TETANUS_MODEL__DEFAULT", "deepseek-v4-pro"),
        ("TETANUS_AGENT__MAX_PARALLEL_TOOL_CALLS", "4"),
        ("TETANUS_LLM__RETRY__BACKOFF__INITIAL_DELAY_MS", "250"),
    ]);

    assert_eq!(document["model.default"], json!("deepseek-v4-pro"));
    assert_eq!(document["agent.max_parallel_tool_calls"], json!(4));
    assert_eq!(document["llm.retry.backoff.initial_delay_ms"], json!(250));
    assert_eq!(document.len(), 3);
}

/// TC-ENV-2: a variable that names no key sets nothing.
///
/// Input: a variable without the prefix, the harness home, a bare prefixed
/// name with no separator, and names with an empty segment at either end or in
/// the middle.
/// Expected: an empty document. `TETANUS_HOME` is the case with teeth: every
/// settled key is sectioned, so a bare name names no key that could exist, and
/// without that rule the harness home would quietly become a setting called
/// `home` that nothing reads and `tetanus config` would report.
#[test]
fn a_variable_that_names_no_key_sets_nothing() {
    let document = from_vars([
        ("PATH", "/usr/bin"),
        ("HOME", "/root"),
        ("DEEPSEEK_API_KEY", "sk-secret"),
        ("TETANUS_HOME", "/somewhere/else"),
        ("TETANUS_SOMETHING", "value"),
        ("TETANUS___LEADING", "value"),
        ("TETANUS_TRAILING__", "value"),
        ("TETANUS_A____B", "value"),
    ]);

    assert!(document.is_empty(), "set: {document:?}");
}

/// TC-ENV-3: a value is JSON when it parses, and text otherwise.
///
/// Input: an integer, a float, both booleans, null, an array, an object, and
/// several plain strings including one with spaces and one that is empty.
/// Expected: the JSON ones arrive as their types, so a key that takes a number
/// gets a number; the rest arrive as strings. Without this a numeric setting
/// supplied by environment would fail type resolution with "must be an
/// integer, not a string", which is a confusing way to learn the mechanism
/// works.
#[test]
fn a_value_is_json_when_it_parses_and_text_otherwise() {
    let document = from_vars([
        ("TETANUS_A__INT", "8"),
        ("TETANUS_A__FLOAT", "0.25"),
        ("TETANUS_A__YES", "true"),
        ("TETANUS_A__NO", "false"),
        ("TETANUS_A__NOTHING", "null"),
        ("TETANUS_A__LIST", r#"["read","write"]"#),
        ("TETANUS_A__MAP", r#"{"mode":"always"}"#),
        ("TETANUS_A__WORD", "debug"),
        ("TETANUS_A__PHRASE", "two words"),
        ("TETANUS_A__EMPTY", ""),
        ("TETANUS_A__PATHISH", "/var/lib/tetanus"),
    ]);

    assert_eq!(document["a.int"], json!(8));
    assert_eq!(document["a.float"], json!(0.25));
    assert_eq!(document["a.yes"], json!(true));
    assert_eq!(document["a.no"], json!(false));
    assert_eq!(document["a.nothing"], json!(null));
    assert_eq!(document["a.list"], json!(["read", "write"]));
    assert_eq!(document["a.map"], json!({ "mode": "always" }));
    assert_eq!(document["a.word"], json!("debug"));
    assert_eq!(document["a.phrase"], json!("two words"));
    assert_eq!(document["a.empty"], json!(""));
    assert_eq!(document["a.pathish"], json!("/var/lib/tetanus"));
}

/// TC-ENV-4: a value that looks like JSON but is meant as text can say so.
///
/// The cost of TC-ENV-3's rule, stated as a case rather than left to be
/// discovered by whoever names a model `123`.
///
/// Input: a bare numeric string, and the same value quoted as a JSON string.
/// Expected: the bare one is a number and the quoted one is text - so the
/// escape exists, and a reader who hits the surprise has somewhere to look.
#[test]
fn a_value_that_looks_like_json_can_be_forced_to_text() {
    let document = from_vars([
        ("TETANUS_MODEL__DEFAULT", "123"),
        ("TETANUS_MODEL__FALLBACK", r#""123""#),
    ]);

    assert_eq!(document["model.default"], json!(123), "the bargain");
    assert_eq!(document["model.fallback"], json!("123"), "and the escape");
}

/// TC-ENV-5: the environment sits above the file and below a flag.
///
/// This is the point of the layer, and it is asserted through `Config` rather
/// than by reading the enum's order, because what matters is which value a
/// reader gets.
///
/// Input: one key set on all four layers, one set only on defaults and
/// environment, and one only on environment.
/// Expected: the flag wins where it is set, the environment wins over the file
/// and the defaults, and every key reports the layer that actually supplied
/// it - so `tetanus config` can finally say `env`.
#[test]
fn the_environment_sits_above_the_file_and_below_a_flag() {
    let mut config = Config::default();
    config.set("a.all", json!("from-default"), Layer::Default);
    config.set("a.all", json!("from-file"), Layer::File);
    config.set("a.all", json!("from-flag"), Layer::Flag);
    config.set("b.some", json!("from-default"), Layer::Default);
    config.load(
        Layer::Env,
        from_vars([
            ("TETANUS_A__ALL", "from-env"),
            ("TETANUS_B__SOME", "from-env"),
            ("TETANUS_C__ONLY", "from-env"),
        ]),
    );

    let resolved = |key: &str| {
        let entry = config.get(key).expect("resolved");
        (entry.value.clone(), entry.layer)
    };

    assert_eq!(resolved("a.all"), (json!("from-flag"), Layer::Flag));
    assert_eq!(resolved("b.some"), (json!("from-env"), Layer::Env));
    assert_eq!(resolved("c.only"), (json!("from-env"), Layer::Env));
}

/// TC-ENV-6: re-reading the environment drops what it no longer sets.
///
/// The layer is loaded whole, like the file layer, so a key the environment
/// used to set and no longer does falls back rather than lingering. A layer
/// that only ever accumulated would make an unset variable indistinguishable
/// from one that was never set.
///
/// Input: a key set on defaults and by the environment, then the environment
/// loaded again without it.
/// Expected: the value falls back to the default and reports that layer.
#[test]
fn re_reading_the_environment_drops_what_it_no_longer_sets() {
    let mut config = Config::default();
    config.set("a.key", json!("from-default"), Layer::Default);
    config.load(Layer::Env, from_vars([("TETANUS_A__KEY", "from-env")]));
    assert_eq!(config.get("a.key").expect("resolved").layer, Layer::Env);

    config.load(Layer::Env, from_vars(Vec::<(String, String)>::new()));

    let entry = config.get("a.key").expect("still resolved");
    assert_eq!(entry.value, json!("from-default"));
    assert_eq!(entry.layer, Layer::Default);
}

/// TC-ENV-7: a credential supplied by environment lands where the redaction
/// rule already covers it.
///
/// Supplying a provider key by environment is the ordinary way to run a
/// container, and it must not become a way to get a credential printed. The
/// naming rule that hides one in a document reads the key, not the layer, so
/// this asserts the two meet rather than assuming they do.
///
/// Input: a provider credential and its non-secret neighbour, both from the
/// environment.
/// Expected: both resolve; the credential is one the redaction rule catches
/// and the neighbour is not.
#[test]
fn a_credential_from_the_environment_is_still_a_credential() {
    let document = from_vars([
        ("TETANUS_LLM__PROVIDERS__DEEPSEEK__API_KEY", "sk-live-xyz"),
        ("TETANUS_LLM__PROVIDERS__DEEPSEEK__API_KEY_ENV", "OTHER_VAR"),
    ]);

    assert_eq!(
        document["llm.providers.deepseek.api_key"],
        json!("sk-live-xyz")
    );
    assert!(
        tetanus_config::secret::names_a_secret("llm.providers.deepseek.api_key"),
        "an environment-supplied credential is withheld by the same rule as a written one"
    );
    assert!(
        !tetanus_config::secret::names_a_secret("llm.providers.deepseek.api_key_env"),
        "and the variable name beside it is not a credential"
    );
}

/// TC-ENV-8: the constants are the ones the documentation and the messages
/// use.
///
/// A prefix or separator that drifted from what is written down would make
/// every instruction wrong at once, and nothing else in the suite would
/// notice.
#[test]
fn the_prefix_and_separator_are_what_is_documented() {
    assert_eq!(PREFIX, "TETANUS_");
    assert_eq!(SEPARATOR, "__");

    // And they compose the way the documentation spells it.
    let document = from_vars([(format!("{PREFIX}LOG{SEPARATOR}LEVEL"), "debug")]);
    assert_eq!(document["log.level"], json!("debug"));
}
