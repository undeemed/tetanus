//! Test Design Specification: the model-facing filesystem tools, ported.
//!
//! Feature under test: `tetanus_fs::tools` - the seven tools, their schemas,
//! the concurrency class each call is scheduled by, the window a read renders,
//! and the shape of a refusal the model reads. Upstream pins the same surface
//! in `packages/fs/tool-fs/tests/tools.spec.ts`, its `error.spec.ts`, and
//! `packages/fs/tool-fs-search/tests/tools.spec.ts`.
//!
//! Approach: the tools through `ToolRegistry::execute`, which is the path the
//! turn engine takes, against a real workspace. Asserting on a tool object
//! directly would skip the registry, and the registry is where a schema, a
//! name and a concurrency class actually take effect.
//!
//! What is not restated, and why. Upstream's `read_image` and its attachment
//! store have no counterpart in this workspace; its rendered `ReadResultView`
//! and diff presentation belong to the presentation lane by
//! `docs/interface-contract.md` §5, so what is asserted here is the text a
//! model reads and not how a surface draws it; and its `grep` tool shells out
//! to `ripgrep`, which `crates/fs/src/glob.rs` explains tetanus does not.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Fixture;
use tetanus_fs::observation::ObservedState;
use tetanus_fs::tools::READ_LIMIT;
use tetanus_fs::FsTools;
use tetanus_turn::schema::violations;
use tetanus_turn::tools::{ToolCall, ToolMode, ToolOutcome, ToolRegistry};

fn registry(fixture: &Fixture) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    FsTools::new(
        fixture.sandboxed(),
        Arc::new(ObservedState::new()),
        "session-a",
    )
    .register(&mut registry);
    registry
}

async fn run(registry: &ToolRegistry, name: &str, arguments: serde_json::Value) -> ToolOutcome {
    registry
        .execute(&ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
        })
        .await
        .expect("the tool answered rather than failing the step")
}

/// TC-PORT-FS-37: the roster the model is offered, and its schemas.
///
/// Upstream: the tool suite registers a fixed set of named tools with declared
/// parameters.
///
/// Input: a registry with the suite composed.
/// Expected: exactly the eight names, each with an object schema that declares
/// the arguments the tool actually reads. The roster is asserted whole so a
/// tool added or renamed is a decision somebody made rather than a change that
/// slipped in.
#[tokio::test]
async fn the_suite_offers_eight_named_tools_with_declared_arguments() {
    let fixture = Fixture::new();
    let registry = registry(&fixture);

    let schemas = registry.schemas();

    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["delete", "edit", "glob", "list", "read", "search", "stat", "write"]
    );
    assert_eq!(names, FsTools::NAMES);
    for schema in &schemas {
        assert_eq!(schema.parameters["type"], "object", "{}", schema.name);
        assert!(
            !schema.description.is_empty(),
            "{} says what it is for",
            schema.name
        );
    }
    let write = schemas.iter().find(|s| s.name == "write").expect("write");
    assert_eq!(write.parameters["required"], json!(["path", "content"]));
}

/// TC-PORT-FS-38: a call that matches the published schema is the call the
/// tool reads.
///
/// Upstream: schemas are the contract between the model and the tool body.
///
/// Input: each tool's minimal well-formed arguments, checked against its own
/// published schema by the engine's validator.
/// Expected: no violations. A schema that disagreed with the body would refuse
/// calls the tool would have served, and the model has no way to discover that.
#[tokio::test]
async fn the_published_schemas_accept_the_calls_the_tools_serve() {
    let fixture = Fixture::new();
    let registry = registry(&fixture);
    let minimal = [
        ("read", json!({ "path": "a.txt" })),
        ("write", json!({ "path": "a.txt", "content": "x" })),
        (
            "edit",
            json!({ "path": "a.txt", "old_string": "x", "new_string": "y" }),
        ),
        ("list", json!({})),
        ("glob", json!({ "pattern": "**/*.rs" })),
        ("stat", json!({ "path": "a.txt" })),
        ("delete", json!({ "path": "a.txt", "recursive": false })),
    ];

    for (name, arguments) in minimal {
        let schema = registry
            .schemas()
            .into_iter()
            .find(|s| s.name == name)
            .expect("registered");
        assert_eq!(
            violations(&schema.parameters, &arguments),
            Vec::<String>::new(),
            "{name} refuses a call it serves"
        );
    }
}

/// TC-PORT-FS-39: reads overlap, mutations do not.
///
/// Upstream: `isConcurrencySafe` per call, and the loop's barrier behaviour.
///
/// Input: each tool classified through the registry, which is what the
/// scheduler asks.
/// Expected: read, list, glob and stat parallel; write, edit and delete
/// exclusive. Two writes to one file overlapping is a lost update, and the
/// classification is the only thing standing between the model and one.
#[tokio::test]
async fn reads_may_overlap_and_mutations_run_alone() {
    let fixture = Fixture::new();
    let registry = registry(&fixture);
    let mode = |name: &str| {
        registry.mode(&ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: json!({ "path": "a.txt" }),
        })
    };

    for name in ["read", "list", "glob", "stat"] {
        assert_eq!(mode(name), ToolMode::Parallel, "{name}");
    }
    for name in ["write", "edit", "delete"] {
        assert_eq!(mode(name), ToolMode::Exclusive, "{name}");
    }
}

/// TC-PORT-FS-40: a read renders numbered lines under a header.
///
/// Upstream: "Results include line numbers".
///
/// Input: a three-line file read whole.
/// Expected: a header naming the file and its length, then one numbered line
/// each. The numbers are what let a model quote a location back, and the header
/// is what tells it the window was the whole file.
#[tokio::test]
async fn a_read_answers_a_header_and_numbered_lines() {
    let fixture = Fixture::new();
    fixture.write("code.rs", "one\ntwo\nthree\n");
    let registry = registry(&fixture);

    let outcome = run(&registry, "read", json!({ "path": "code.rs" })).await;

    assert!(outcome.ok);
    let lines: Vec<&str> = outcome.content.lines().collect();
    assert_eq!(lines[0], "code.rs (3 lines)");
    assert_eq!(lines[1], "     1\tone");
    assert_eq!(lines[3], "     3\tthree");
}

/// TC-PORT-FS-41: a window says which part of the file it is, and how to
/// continue.
///
/// Upstream: `offset`/`limit` with the deployment's line cap.
///
/// Input: a hundred-line file read from line 10 with a limit of 5, and then a
/// read starting past the end.
/// Expected: the header names the range and the total; a trailing line says how
/// to read on; a start past the end is answered plainly rather than as an
/// error. A model that cannot tell a window from a whole file concludes the
/// rest of the file does not exist.
#[tokio::test]
async fn a_window_names_its_range_and_says_how_to_continue() {
    let fixture = Fixture::new();
    let body: String = (1..=100).map(|n| format!("line {n}\n")).collect();
    fixture.write("long.txt", &body);
    let registry = registry(&fixture);

    let window = run(
        &registry,
        "read",
        json!({ "path": "long.txt", "offset": 10, "limit": 5 }),
    )
    .await;
    let past = run(
        &registry,
        "read",
        json!({ "path": "long.txt", "offset": 500 }),
    )
    .await;

    let lines: Vec<&str> = window.content.lines().collect();
    assert_eq!(lines[0], "long.txt (lines 10-14 of 100)");
    assert_eq!(lines[1], "    10\tline 10");
    assert_eq!(lines.len(), 7, "header, five lines, and the continuation");
    assert!(lines[6].contains("read again from line 15"), "{}", lines[6]);
    assert!(past.ok, "a start past the end is a fact, not a failure");
    assert!(past.content.contains("past the end"), "{}", past.content);
}

/// TC-PORT-FS-42: a limit past the cap is clamped rather than refused.
///
/// Upstream: "limit must be less than or equal to `readLimit`", refused.
///
/// Input: a read asking for ten times the cap.
/// Expected: the call succeeds and returns at most the cap. tetanus differs
/// from upstream here on purpose: a model that asked for too much wanted the
/// file, and answering with the cap gets it moving, where a refusal costs a
/// round trip to learn a number it could not have known.
#[tokio::test]
async fn a_limit_past_the_cap_is_clamped_to_the_cap() {
    let fixture = Fixture::new();
    let body: String = (1..=50).map(|n| format!("line {n}\n")).collect();
    fixture.write("long.txt", &body);
    let registry = registry(&fixture);

    let outcome = run(
        &registry,
        "read",
        json!({ "path": "long.txt", "limit": READ_LIMIT * 10 }),
    )
    .await;

    assert!(outcome.ok);
    assert_eq!(outcome.content.lines().count(), 51, "header plus 50 lines");
}

/// TC-PORT-FS-43: every refusal reaches the model as its class then its
/// sentence.
///
/// Upstream: "the tool registry exposes `{ name, code }` on `isError` results
/// so retry/permission/UI layers can branch without parsing messages".
///
/// Input: a read of a missing file, a write outside the workspace, and an edit
/// whose text does not occur.
/// Expected: `ok: false` with the code first, then the sentence. A surface
/// finds the class at a fixed position; the model reads past it to the part
/// that says what to do. Crucially the *turn* does not fail: a denied path must
/// leave the model able to try something else.
#[tokio::test]
async fn a_refusal_is_a_result_carrying_its_class_and_its_advice() {
    let fixture = Fixture::new();
    fixture.write("code.rs", "let x = 1;\n");
    let registry = registry(&fixture);
    let outside = fixture.outside().join("escape.txt").display().to_string();

    let missing = run(&registry, "read", json!({ "path": "gone.txt" })).await;
    let escaped = run(
        &registry,
        "write",
        json!({ "path": outside, "content": "x" }),
    )
    .await;
    run(&registry, "read", json!({ "path": "code.rs" })).await;
    let nomatch = run(
        &registry,
        "edit",
        json!({ "path": "code.rs", "old_string": "nowhere", "new_string": "x" }),
    )
    .await;

    for outcome in [&missing, &escaped, &nomatch] {
        assert!(!outcome.ok, "a refusal is a result with ok: false");
    }
    assert!(
        missing.content.starts_with("FS_NOT_FOUND: "),
        "{}",
        missing.content
    );
    assert!(
        escaped.content.starts_with("FS_SANDBOX_DENIED: "),
        "{}",
        escaped.content
    );
    assert!(
        escaped.content.contains("Work inside the workspace"),
        "{}",
        escaped.content
    );
    assert!(
        nomatch.content.starts_with("FS_EDIT_NOT_FOUND: "),
        "{}",
        nomatch.content
    );
}

/// TC-PORT-FS-44: arguments the schema would have caught are answered, not
/// panicked on.
///
/// Upstream: argument validation refuses the call with a message.
///
/// Input: a read with no path, and a read whose offset is zero.
/// Expected: both come back as errors naming the argument. The pipeline checks
/// against the schema first, so reaching these means a caller dispatched
/// without that check - and a tool that panicked there would take the turn down
/// with it.
#[tokio::test]
async fn bad_arguments_are_reported_against_the_argument_that_is_wrong() {
    let fixture = Fixture::new();
    let registry = registry(&fixture);

    let missing = registry
        .execute(&ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({}),
        })
        .await
        .unwrap_err();
    let zero = registry
        .execute(&ToolCall {
            id: "c2".into(),
            name: "read".into(),
            arguments: json!({ "path": "a.txt", "offset": 0 }),
        })
        .await
        .unwrap_err();

    assert!(missing.to_string().contains("`path`"), "{missing}");
    assert!(zero.to_string().contains("`offset`"), "{zero}");
}

/// TC-PORT-FS-45: the listing, glob and stat tools say what they found in
/// words a model can act on.
///
/// Upstream: listings are metadata and content-free; a search answers paths.
///
/// Input: a small tree, listed, globbed and statted.
/// Expected: the listing marks directories and sizes files; the glob answers
/// workspace-relative paths one per line; a stat of a missing path says so
/// rather than failing. Each answer is the shortest thing that still tells the
/// model what to do next.
#[tokio::test]
async fn listing_globbing_and_statting_answer_in_words_a_model_can_use() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "fn main() {}\n");
    fixture.write("README.md", "# hi\n");
    let registry = registry(&fixture);

    let listed = run(&registry, "list", json!({})).await;
    let globbed = run(&registry, "glob", json!({ "pattern": "**/*.rs" })).await;
    let file = run(&registry, "stat", json!({ "path": "README.md" })).await;
    let missing = run(&registry, "stat", json!({ "path": "nope.md" })).await;

    assert!(
        listed.content.contains("README.md (5 bytes)"),
        "{}",
        listed.content
    );
    assert!(listed.content.contains("src/"), "{}", listed.content);
    assert_eq!(globbed.content, "src/main.rs");
    assert_eq!(file.content, "README.md is a file of 5 bytes");
    assert!(missing.ok, "an absence is an answer, not a failure");
    assert_eq!(missing.content, "nope.md does not exist");
}

/// TC-PORT-FS-46: a very long line is clipped, and the clip is marked.
///
/// Upstream: `readMaxLineLength` bounds one line.
///
/// Input: a file whose single line is far past the bound.
/// Expected: the line is cut at the bound and marked as cut. A model handed an
/// unmarked clipped line edits against text that is not what is in the file,
/// and the edit then fails to match for a reason it cannot see.
#[tokio::test]
async fn a_line_past_the_bound_is_clipped_and_says_so() {
    let fixture = Fixture::new();
    fixture.write("bundle.js", &format!("{}\n", "x".repeat(9000)));
    let registry = registry(&fixture);

    let outcome = run(&registry, "read", json!({ "path": "bundle.js" })).await;

    assert!(outcome.ok);
    assert!(
        outcome.content.contains("[line truncated at"),
        "the clip is marked"
    );
    assert!(
        outcome.content.len() < 9000,
        "the clipped line is shorter than the line"
    );
}

/// TC-PORT-FS-47: writing and deleting through the tools does what it says.
///
/// Upstream: the write tool's create/update reporting.
///
/// Input: a create, an update after a read, and a delete.
/// Expected: each answer names the operation and the path, and the disk agrees.
/// The wording is asserted because it is what the model reasons from: "created"
/// and "updated" are different facts about whether something was already there.
#[tokio::test]
async fn a_write_and_a_delete_report_what_they_did() {
    let fixture = Fixture::new();
    let registry = registry(&fixture);

    let created = run(
        &registry,
        "write",
        json!({ "path": "notes.md", "content": "one\ntwo\n" }),
    )
    .await;
    let updated = run(
        &registry,
        "write",
        json!({ "path": "notes.md", "content": "one\n" }),
    )
    .await;
    let deleted = run(&registry, "delete", json!({ "path": "notes.md" })).await;

    assert_eq!(created.content, "created notes.md (2 lines, 8 bytes)");
    assert_eq!(updated.content, "updated notes.md (1 lines, 4 bytes)");
    assert_eq!(deleted.content, "deleted notes.md");
    assert!(!fixture.exists("notes.md"));
}

/// TC-PORT-FS-51: a search answers with the matching lines, each named by its
/// file and line number.
///
/// Input: three files, two of which contain the word searched for.
/// Expected: one line per match, `path:line: text`, with a header counting the
/// lines and the files. Upstream's `grep` shells out to `ripgrep` and renders
/// the same three facts; what is restated is the answer, not the mechanism.
#[tokio::test]
async fn a_search_answers_with_the_lines_that_match_and_where_they_are() {
    let fixture = Fixture::new();
    fixture.write("src/one.rs", "fn alpha() {}\nfn beta() {}\n");
    fixture.write("src/two.rs", "// nothing here\n");
    fixture.write("src/three.rs", "fn gamma() {}\n");
    let registry = registry(&fixture);

    let found = run(&registry, "search", json!({ "pattern": r"fn \w+" })).await;

    assert!(found.ok, "{}", found.content);
    let lines: Vec<&str> = found.content.lines().collect();
    assert_eq!(lines[0], "3 matching lines in 2 files");
    assert!(lines.contains(&"src/one.rs:1: fn alpha() {}"), "{lines:?}");
    assert!(lines.contains(&"src/one.rs:2: fn beta() {}"), "{lines:?}");
    assert!(
        lines.contains(&"src/three.rs:1: fn gamma() {}"),
        "{lines:?}"
    );
    assert!(
        !found.content.contains("two.rs"),
        "a file with no match is not named: {}",
        found.content
    );
}

/// TC-PORT-FS-52: the glob narrows what a search opens, and no match is an
/// answer rather than a failure.
///
/// Input: the same word in two file types, searched under one glob; then a
/// pattern nothing matches.
/// Expected: only the globbed file is reported; the empty search is `ok` and
/// says so in words, because "I looked and there is nothing" is a result a
/// model acts on and a failure is something it retries.
#[tokio::test]
async fn a_glob_narrows_the_search_and_no_match_is_still_an_answer() {
    let fixture = Fixture::new();
    fixture.write("keep.rs", "target\n");
    fixture.write("skip.md", "target\n");
    let registry = registry(&fixture);

    let narrowed = run(
        &registry,
        "search",
        json!({ "pattern": "target", "glob": "**/*.rs" }),
    )
    .await;
    let missing = run(&registry, "search", json!({ "pattern": "absent" })).await;

    assert!(narrowed.content.contains("keep.rs:1: target"));
    assert!(
        !narrowed.content.contains("skip.md"),
        "{}",
        narrowed.content
    );
    assert!(missing.ok, "an empty search is not a failure");
    assert!(
        missing.content.starts_with("no line under"),
        "{}",
        missing.content
    );
}

/// TC-PORT-FS-53: case folding is the default, and the argument turns it off.
///
/// Input: `Target` searched for as `target`, with and without
/// `case_sensitive`.
/// Expected: found by default, not found when the case must match. The default
/// is the forgiving one because a model that wanted exactness can ask for it,
/// while a model that missed a match learns nothing from the miss.
#[tokio::test]
async fn a_search_folds_case_unless_it_is_told_not_to() {
    let fixture = Fixture::new();
    fixture.write("notes.md", "Target acquired\n");
    let registry = registry(&fixture);

    let folded = run(&registry, "search", json!({ "pattern": "target" })).await;
    let exact = run(
        &registry,
        "search",
        json!({ "pattern": "target", "case_sensitive": true }),
    )
    .await;

    assert!(folded.content.contains("notes.md:1: Target acquired"));
    assert!(
        exact.content.starts_with("no line under"),
        "{}",
        exact.content
    );
}

/// TC-PORT-FS-54: a file the search cannot read is skipped, counted and
/// reported - never fatal, and never silent.
///
/// Input: a text file that matches beside a file of bytes that are not UTF-8.
/// Expected: the match is answered, the call is `ok`, and the answer says one
/// file was skipped. Silence is the failure mode this case exists for: a
/// search that quietly steps over a file has answered "no matches" about
/// something it never looked at.
#[tokio::test]
async fn a_binary_file_is_skipped_and_the_answer_says_so() {
    let fixture = Fixture::new();
    fixture.write("readable.txt", "needle\n");
    std::fs::write(fixture.root().join("blob.bin"), [0xff, 0xfe, 0x00, 0x01])
        .expect("write the binary file");
    let registry = registry(&fixture);

    let found = run(&registry, "search", json!({ "pattern": "needle" })).await;

    assert!(found.ok, "{}", found.content);
    assert!(found.content.contains("readable.txt:1: needle"));
    assert!(
        found
            .content
            .contains("1 unreadable or non-text file skipped"),
        "{}",
        found.content
    );
}

/// TC-PORT-FS-55: a pattern that is not a regular expression is the model's
/// mistake, answered with the reason and not with a panic.
///
/// Input: an unclosed group.
/// Expected: `ok: false`, the class first, and the regex engine's own
/// explanation after it - which names the offset, and is what makes the next
/// attempt different from this one.
#[tokio::test]
async fn a_malformed_pattern_is_refused_in_words() {
    let fixture = Fixture::new();
    fixture.write("file.txt", "text\n");
    let registry = registry(&fixture);

    let refused = run(&registry, "search", json!({ "pattern": "fn (" })).await;

    assert!(!refused.ok);
    assert!(
        refused.content.starts_with("FS_BAD_PATTERN:"),
        "{}",
        refused.content
    );
}

/// TC-PORT-FS-56: a search is bounded, and says that it stopped.
///
/// Input: more matching lines than the bound.
/// Expected: exactly the bound is returned and the last line says the search
/// stopped. A search whose whole point is to be cheaper than reading the files
/// must not answer with more text than the files.
#[tokio::test]
async fn a_search_stops_at_its_bound_and_says_so() {
    let fixture = Fixture::new();
    let many: String = (0..tetanus_fs::tools::MAX_SEARCH_MATCHES + 40)
        .map(|n| format!("hit {n}\n"))
        .collect();
    fixture.write("many.txt", &many);
    let registry = registry(&fixture);

    let found = run(&registry, "search", json!({ "pattern": "^hit" })).await;

    let lines: Vec<&str> = found.content.lines().collect();
    assert_eq!(
        lines.len(),
        tetanus_fs::tools::MAX_SEARCH_MATCHES + 2,
        "a header, the bound in matches, and the notice"
    );
    assert!(
        lines.last().expect("a last line").contains("stopped at"),
        "{:?}",
        lines.last()
    );
}

/// TC-PORT-FS-57: the fence judges a search, and a search does not license a
/// write.
///
/// Input: a search under a path outside the workspace; then a search that
/// matches a file, followed by a write to that file.
/// Expected: the outside search is refused by the fence like any other
/// resolution, and the write is still refused as unobserved. The second half
/// is the decision worth pinning: a search shows a model a handful of lines
/// out of a file it has otherwise never seen, so letting that count as reading
/// would let a model grep for one word and then replace everything.
#[tokio::test]
async fn the_fence_judges_a_search_and_a_search_is_not_a_read() {
    let fixture = Fixture::new();
    fixture.write("owned.txt", "needle\nrest\n");
    let registry = registry(&fixture);

    let outside = run(
        &registry,
        "search",
        json!({ "pattern": "needle", "path": fixture.outside().display().to_string() }),
    )
    .await;
    let found = run(&registry, "search", json!({ "pattern": "needle" })).await;
    let overwrite = run(
        &registry,
        "write",
        json!({ "path": "owned.txt", "content": "replaced\n" }),
    )
    .await;

    assert!(!outside.ok, "{}", outside.content);
    assert!(
        outside.content.starts_with("FS_SANDBOX_DENIED:"),
        "{}",
        outside.content
    );
    assert!(found.content.contains("owned.txt:1: needle"));
    assert!(!overwrite.ok, "a search must not license an overwrite");
    assert!(
        overwrite.content.starts_with("FS_NOT_OBSERVED:"),
        "{}",
        overwrite.content
    );
    assert_eq!(fixture.read("owned.txt"), "needle\nrest\n");
}
