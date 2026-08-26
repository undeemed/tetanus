//! Test Design Specification: the model providers a document declares.
//!
//! Features under test: [`tetanus_engine::providers::custom_providers`], which
//! reads `llm.providers.<name>.*` out of the settings document, and
//! [`tetanus_engine::providers::ProviderSet`], the registry every surface
//! routes through. Together they are what makes a provider a line of
//! configuration instead of a line of Rust.
//!
//! Approach: every case reads a real document off disk, for the reason
//! `retry.rs` gives - the keys are only right if a reader's flattening
//! produces them from the nesting somebody would actually write. A case that
//! set a flat key directly would pass on a document nobody can author.
//!
//! Features NOT tested here: the wire format of a request to such a provider
//! (`crates/turn/tests/openai_compat_adapter.rs`), the retry block written
//! under the same namespace (`retry.rs`), and what the CLI does with a name
//! (`crates/cli/tests/custom_provider.rs`). None is restated.
//!
//! Environmental needs: a writable temp directory. TC-PROV-15 reads and writes
//! one environment variable of its own; nothing else touches the environment
//! and no case opens a socket.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_config::ConfigError;
use tetanus_engine::agent::Providers;
use tetanus_engine::catalog::Catalogs;
use tetanus_engine::providers::{custom_providers, ProviderSet, RESERVED};
use tetanus_engine::{boot, EngineConfig};

/// A document on disk, read the way the boot reads it.
fn document(text: &str) -> (TempDir, tetanus_config::Config) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, text).expect("write");
    let settings = boot::document(&path).expect("read");
    (dir, settings)
}

/// The blocks a document declares, or the failure it is refused with.
fn declared(
    text: &str,
) -> Result<Vec<(String, tetanus_turn::llm::deepseek::DeepSeekConfig)>, ConfigError> {
    let (_dir, settings) = document(text);
    custom_providers(&settings)
}

/// A minimal well-formed block, for a case that is about something else.
const ONE: &str = "llm:
  providers:
    local:
      base_url: http://127.0.0.1:11434/v1
      api_key_env: LOCAL_KEY
";

/// The key a `ConfigError::BadValue` names, so a case asserts on the key
/// rather than on a rendered sentence.
fn blamed(error: &ConfigError) -> String {
    match error {
        ConfigError::BadValue { key, .. } => key.clone(),
        other => panic!("expected a bad value, got {other:?}"),
    }
}

/// TC-PROV-1: two blocks in one document, both read, in one settled order.
///
/// Input: a document declaring `alpha` and `zeta`, written in that order.
/// Expected: both are returned with the values they were written with, sorted
/// by name. The order is the order a picker lists them in, so it has to be a
/// property of the document's content and not of the reader's iteration.
#[test]
fn two_blocks_are_both_read_in_a_settled_order() {
    let declared = declared(
        "llm:
  providers:
    zeta:
      base_url: https://zeta.example/v1
      api_key_env: ZETA_KEY
    alpha:
      base_url: https://alpha.example/v1
      api_key_env: ALPHA_KEY
",
    )
    .expect("two blocks");

    let names: Vec<&str> = declared.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert_eq!(declared[0].1.base_url, "https://alpha.example/v1");
    assert_eq!(declared[0].1.api_key_env, "ALPHA_KEY");
    assert_eq!(declared[1].1.base_url, "https://zeta.example/v1");
}

/// TC-PROV-2: a document with no provider block declares no provider.
///
/// Input: a document that configures the general retry block only.
/// Expected: an empty list. The defaults layer writes the general block's
/// keys, so a reader that did not tell the two namespaces apart would find a
/// provider in every document ever written, including the empty one.
#[test]
fn a_document_with_no_block_declares_nothing() {
    assert!(declared("llm:\n  retry:\n    mode: always\n")
        .expect("no blocks")
        .is_empty());
    assert!(declared("sessions:\n  root: /tmp/x\n")
        .expect("no blocks")
        .is_empty());
}

/// TC-PROV-3: every optional field, set.
///
/// Input: a block writing `models`, `max_tokens` and both timeouts.
/// Expected: each reaches the adapter config unchanged, and the two timeouts
/// reach the bounds the transport will run with. A timeout that was read and
/// dropped is a route running on the compiled default while its document says
/// otherwise, which nothing downstream can notice.
#[test]
fn every_optional_field_is_read() {
    let declared = declared(
        "llm:
  providers:
    gateway:
      base_url: https://gateway.example/v1
      api_key_env: GATEWAY_KEY
      models: [big, small]
      max_tokens: 512
      stream_idle_timeout_ms: 4000
      request_deadline_ms: 9000
",
    )
    .expect("one block");

    let (name, config) = &declared[0];
    assert_eq!(name, "gateway");
    assert_eq!(config.models, ["big", "small"]);
    assert_eq!(config.max_tokens, Some(512));
    assert_eq!(config.idle_window().as_millis(), 4000);
    assert_eq!(config.deadline().as_millis(), 9000);
}

/// TC-PROV-4: every optional field, unset.
///
/// Input: the two required keys and nothing else.
/// Expected: no models, no token cap, and the transport's own two bounds. An
/// empty catalogue is a provider that names no models rather than one that
/// serves none - the panel and `tetanus models` both already word it that way
/// - so it is a default and not a fault.
#[test]
fn the_optional_fields_have_defaults() {
    let declared = declared(ONE).expect("one block");

    let (name, config) = &declared[0];
    assert_eq!(name, "local");
    assert!(config.models.is_empty());
    assert_eq!(config.max_tokens, None);
    assert_eq!(
        config.idle_window().as_millis(),
        u128::from(tetanus_turn::llm::deepseek::DEFAULT_STREAM_IDLE_TIMEOUT_MS)
    );
    assert_eq!(
        config.deadline().as_millis(),
        u128::from(tetanus_turn::llm::deepseek::DEFAULT_REQUEST_DEADLINE_MS)
    );
}

/// TC-PROV-5: a timeout written as zero is the adapter's default.
///
/// Input: both timeouts set to `0`.
/// Expected: the compiled defaults, not a window of no time. The adapter reads
/// a zero this way already; refusing it here would make "leave it alone" spell
/// differently in a document than it does in the code that reads it.
#[test]
fn a_zero_timeout_is_the_adapters_default() {
    let declared = declared(
        "llm:
  providers:
    local:
      base_url: http://127.0.0.1:11434/v1
      api_key_env: LOCAL_KEY
      stream_idle_timeout_ms: 0
      request_deadline_ms: 0
",
    )
    .expect("one block");

    let config = &declared[0].1;
    assert_eq!(
        config.idle_window().as_millis(),
        u128::from(tetanus_turn::llm::deepseek::DEFAULT_STREAM_IDLE_TIMEOUT_MS)
    );
    assert_eq!(
        config.deadline().as_millis(),
        u128::from(tetanus_turn::llm::deepseek::DEFAULT_REQUEST_DEADLINE_MS)
    );
}

/// TC-PROV-6: a block with no `base_url` is refused, naming the key.
///
/// Input: a block that sets `api_key_env` and nothing else.
/// Expected: `BadValue` on `llm.providers.local.base_url`. There is no default
/// address to fall back to, and inventing one would send a deployment's
/// requests somewhere it never named.
#[test]
fn a_block_with_no_address_is_refused() {
    let error = declared(
        "llm:
  providers:
    local:
      api_key_env: LOCAL_KEY
",
    )
    .expect_err("no base_url");

    assert_eq!(blamed(&error), "llm.providers.local.base_url");
}

/// TC-PROV-7: a block with no `api_key_env`, or a blank one, is refused.
///
/// Input: a block missing the key reference, and one whose value is spaces.
/// Expected: `BadValue` on `llm.providers.local.api_key_env` for both. Blank
/// is refused for the reason an empty name is refused everywhere else in the
/// boot: it is a value somebody wrote that cannot mean what they meant.
#[test]
fn a_block_with_no_credential_reference_is_refused() {
    for text in [
        "llm:
  providers:
    local:
      base_url: http://127.0.0.1:11434/v1
",
        "llm:
  providers:
    local:
      base_url: http://127.0.0.1:11434/v1
      api_key_env: \"   \"
",
    ] {
        let error = declared(text).expect_err("no api_key_env");
        assert_eq!(blamed(&error), "llm.providers.local.api_key_env");
    }
}

/// TC-PROV-8: a value of the wrong kind is refused, for each kind of key.
///
/// Input: a string where a list belongs, a string where a count belongs, a
/// zero token cap, a list where a wait belongs, and a number where an address
/// belongs.
/// Expected: `BadValue` naming that key every time. A wrong-typed value that
/// was ignored would run the harness on a setting its author did not write,
/// which is the rule the whole boot is built on.
#[test]
fn a_value_of_the_wrong_kind_is_refused() {
    for (text, key) in [
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      models: not-a-list
",
            "llm.providers.local.models",
        ),
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      models: [ok, 7]
",
            "llm.providers.local.models",
        ),
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      max_tokens: plenty
",
            "llm.providers.local.max_tokens",
        ),
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      max_tokens: 0
",
            "llm.providers.local.max_tokens",
        ),
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      stream_idle_timeout_ms: [1, 2]
",
            "llm.providers.local.stream_idle_timeout_ms",
        ),
        (
            "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      request_deadline_ms: -1
",
            "llm.providers.local.request_deadline_ms",
        ),
        (
            "llm:
  providers:
    local:
      base_url: 7
      api_key_env: K
",
            "llm.providers.local.base_url",
        ),
    ] {
        let error = declared(text).expect_err("a wrong-typed value");
        assert_eq!(blamed(&error), key, "in:\n{text}");
    }
}

/// TC-PROV-9: a model list one element shorter than it was written is refused.
///
/// Input: a list holding an empty string.
/// Expected: `BadValue` on the list. Dropping it quietly would advertise a
/// catalogue nobody wrote, and the counts are what makes the difference
/// visible - the same rule the retryable-code list is read with.
#[test]
fn a_model_list_that_would_silently_shrink_is_refused() {
    let error = declared(
        "llm:
  providers:
    local:
      base_url: http://x/v1
      api_key_env: K
      models: [good, \"\"]
",
    )
    .expect_err("a blank entry");

    assert_eq!(blamed(&error), "llm.providers.local.models");
}

/// TC-PROV-10: a block written under no name at all is refused.
///
/// Input: a document flattening to `llm.providers..<leaf>`, which is what a
/// block under an empty key produces.
/// Expected: `BadValue` naming a key of that nameless block. Which leaf is
/// named is the first one read and is not a promise; that the block is refused
/// is. A nameless provider cannot be asked for by `--adapter` or picked in the
/// panel, so registering one would add a route nobody can reach.
#[test]
fn a_block_with_no_name_is_refused() {
    let error = declared(
        "llm:
  providers:
    \"\":
      base_url: http://x/v1
      api_key_env: K
",
    )
    .expect_err("an empty name");

    assert!(
        blamed(&error).starts_with("llm.providers.."),
        "{}",
        blamed(&error)
    );
}

/// TC-PROV-11: a name a built-in route already answers to is refused, for each
/// of the three spellings.
///
/// Input: a block named `mock`, then `deepseek`, then `deepseek-official`.
/// Expected: `BadValue` naming a key of that block every time. Two adapters
/// under one name would leave which of them a session gets to list order, and
/// `deepseek` is reserved beside the route itself because that is the alias
/// `--adapter` takes.
#[test]
fn a_name_a_built_in_route_owns_is_refused() {
    for name in RESERVED {
        let error = declared(&format!(
            "llm:
  providers:
    {name}:
      base_url: http://x/v1
      api_key_env: K
"
        ))
        .expect_err("a reserved name");

        assert!(
            blamed(&error).starts_with(&format!("llm.providers.{name}.")),
            "{}",
            blamed(&error)
        );
    }
}

/// TC-PROV-12: keys under the namespace that are not this reader's are
/// ignored, and do not declare a provider.
///
/// Input: a document writing only a `retry` block and an `api_key` under a
/// name, which is exactly what a retry policy plus the credential store
/// produce.
/// Expected: no provider is declared and nothing is refused. Refusing what it
/// does not recognize would make storing a credential a boot failure, and
/// declaring a provider from a retry block would register a route with no
/// address.
#[test]
fn a_key_that_is_not_this_readers_neither_declares_nor_refuses() {
    let declared = declared(
        "llm:
  providers:
    someone:
      retry:
        mode: always
      api_key: sekrit
",
    )
    .expect("nothing to declare");

    assert!(declared.is_empty(), "{declared:?}");
}

/// TC-PROV-17: a scalar written where a provider's block belongs declares
/// nothing.
///
/// Input: `llm.providers.local: off`, which flattens to a key with no leaf
/// after the name.
/// Expected: no provider and no failure. It is not this reader's key - there
/// is nothing under the name to read - and refusing it here would make the
/// provider reader the judge of a namespace it shares with two others.
#[test]
fn a_scalar_where_a_block_belongs_declares_nothing() {
    assert!(declared("llm:\n  providers:\n    local: off\n")
        .expect("nothing to declare")
        .is_empty());
}

/// TC-PROV-13: a declared provider also carrying a retry block is one
/// provider, read whole.
///
/// Input: a block with both this reader's keys and a retry block.
/// Expected: one provider, with its address and catalogue, and the retry
/// reader still finds its policy. The two readers share a namespace and must
/// not shadow each other.
#[test]
fn a_block_may_carry_a_retry_policy_beside_its_route() {
    let (_dir, settings) = document(
        "llm:
  providers:
    gateway:
      base_url: https://gateway.example/v1
      api_key_env: GATEWAY_KEY
      retry:
        mode: always
",
    );

    let declared = custom_providers(&settings).expect("one block");
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].0, "gateway");
    assert_eq!(declared[0].1.base_url, "https://gateway.example/v1");

    let policies = tetanus_engine::retry::provider_policies(&settings).expect("policies");
    assert!(policies.contains_key("gateway"), "{policies:?}");
}

/// TC-PROV-14: the registry lists the built-in routes first, then the
/// document's, and resolves a name to the adapter that owns it.
///
/// Input: a set composed from a document declaring one provider.
/// Expected: `all()` is mock, `deepseek-official`, then `local`; `adapter()`
/// finds each of them by its own route and answers `None` for a name nothing
/// serves. The order is what a picker opens on, so the two routes that need no
/// configuration come first.
#[test]
fn the_registry_lists_the_built_ins_then_the_document_s() {
    let (_dir, settings) = document(ONE);
    let set = ProviderSet::from_settings(&settings).expect("a registry");

    let routes: Vec<String> = set
        .all()
        .into_iter()
        .map(|adapter| adapter.provider().to_string())
        .collect();
    assert_eq!(routes, ["mock", "deepseek-official", "local"]);

    for route in &routes {
        assert_eq!(
            set.adapter(route).expect("registered").provider(),
            route,
            "{route}"
        );
    }
    assert!(set.adapter("nothing-here").is_none());
    assert!(
        set.adapter("deepseek").is_none(),
        "the alias belongs to the CLI, not to the registry"
    );
}

/// TC-PROV-15: the catalogue reports a declared provider, and its availability
/// follows the environment variable its block named.
///
/// Input: an engine booted on a document declaring one provider, read twice -
/// once with the variable unset and once with it exported.
/// Expected: three providers listed with the document's models and credential
/// reference, `available` false and then true. This is the whole contract of
/// `ProviderDescriptor.available`: a picker greys the entry rather than
/// meeting `MissingCredential` on the first turn.
///
/// Environmental needs: one variable, named for this case alone so no other
/// case reads what it writes.
#[test]
fn the_catalogue_follows_the_credential_the_block_named() {
    const ENV: &str = "TETANUS_TEST_PROVIDERS_CATALOG_KEY";
    let (_dir, settings) = document(&format!(
        "llm:
  providers:
    local:
      base_url: http://127.0.0.1:11434/v1
      api_key_env: {ENV}
      models: [stub-model]
"
    ));
    let config = EngineConfig::from_settings(settings).expect("settings");

    std::env::remove_var(ENV);
    let listed = Catalogs::new(&config).models().providers;
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[2].provider, "local");
    assert_eq!(listed[2].models, ["stub-model"]);
    assert_eq!(listed[2].credential_env.as_deref(), Some(ENV));
    assert!(!listed[2].available, "no key, no availability");

    std::env::set_var(ENV, "placeholder");
    let listed = Catalogs::new(&config).models().providers;
    assert!(listed[2].available, "the key was exported");
    std::env::remove_var(ENV);
}

/// TC-PROV-16: a document that declares nothing still boots the two built-in
/// routes, and a document the reader refuses stops the boot.
///
/// Input: an empty document, then one with a broken block.
/// Expected: two routes and no failure; then a `BadValue` out of
/// `EngineConfig::from_settings` naming the key. Wiring the reader into the
/// boot is what puts a provider on every surface at once, so the failure has
/// to arrive there too rather than at the first turn.
#[test]
fn the_boot_composes_the_registry_and_refuses_a_broken_block() {
    let (_dir, settings) = document("sessions:\n  root: /tmp/x\n");
    let config = EngineConfig::from_settings(settings).expect("settings");
    let routes: Vec<String> = config
        .providers
        .all()
        .into_iter()
        .map(|adapter| adapter.provider().to_string())
        .collect();
    assert_eq!(routes, ["mock", "deepseek-official"]);

    let (_dir, broken) = document(
        "llm:
  providers:
    local:
      api_key_env: LOCAL_KEY
",
    );
    // `EngineConfig` holds trait objects and is not `Debug`, so the failure is
    // taken by matching rather than by `expect_err`.
    let Err(error) = EngineConfig::from_settings(broken) else {
        panic!("a broken block booted");
    };
    assert_eq!(blamed(&error), "llm.providers.local.base_url");
}
