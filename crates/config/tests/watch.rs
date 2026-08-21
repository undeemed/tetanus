//! Test Design Specification: noticing that the settings document changed.
//!
//! Feature under test: `tetanus_config::watch::Watcher` - the half of runtime
//! settings reload that says *when*. `recompose` has been able to re-read a
//! document since it was written and nothing ever called it, so a user editing
//! `settings.yaml` while the harness ran saw no effect until a restart.
//!
//! Upstream watches with chokidar under a `stabilityThreshold`; its
//! `watcher.spec.ts` drives a fake watcher by emitting events on it rather
//! than waiting on the operating system. These cases do the same thing one
//! level lower: `observe` takes the observation, so a sequence of file states
//! is written out literally instead of being produced by racing a filesystem
//! whose timestamp granularity is not this seam's business. Two cases do go
//! through real files, because "does a stamp actually change when a file is
//! rewritten" is a question only the filesystem can answer.
//!
//! What is not restated: upstream's dispose-quiesce, in-flight-write and
//! comment-preserving-write cases belong to a service that also writes the
//! document; this only reads. Its debounce is expressed here as a settle
//! count rather than a duration, because the caller owns the interval.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key, and no case sleeps.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::time::{Duration, SystemTime};

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::recompose::recompose;
use tetanus_config::watch::{Stamp, Watcher, DEFAULT_POLL_INTERVAL};
use tetanus_config::{Config, Layer};

/// TC-WATCH-1: a document nobody touches reports nothing, for ever.
///
/// The common case, and the one that must cost nothing: a harness polling a
/// settings file all day should do no work and republish nothing. A watcher
/// that reported its baseline once at startup would make every run reload its
/// own configuration for no reason.
///
/// Input: a watcher started on a state, observing that same state repeatedly.
/// Expected: nothing reported, however many times it is asked.
#[test]
fn an_untouched_document_reports_nothing() {
    let mut watcher = watching(at(1, 100));

    for _ in 0..5 {
        assert_eq!(watcher.observe(at(1, 100)), None);
    }
}

/// TC-WATCH-2: a settled change is reported exactly once.
///
/// Reporting it twice would make a caller re-read and republish a document
/// that had not moved since, which is how a reload loop starts.
///
/// Input: a change, then the new state observed again several times.
/// Expected: the change reported on the poll that saw it, and nothing after -
/// the new state has become the baseline.
#[test]
fn a_settled_change_is_reported_once() {
    let mut watcher = watching(at(1, 100));

    assert_eq!(watcher.observe(at(2, 120)), Some(at(2, 120)));
    for _ in 0..3 {
        assert_eq!(watcher.observe(at(2, 120)), None, "already reported");
    }
}

/// TC-WATCH-3: a document still being written is not reported until it stops.
///
/// This is the case with a real bug behind it. An editor that truncates and
/// rewrites is momentarily an empty file, so a watcher that fired on the first
/// sighting hands `recompose` an empty document - which parses, drops every
/// key, and gives a running harness the defaults. Waiting for one quiet
/// observation removes that.
///
/// Input: a settle count of two, then the truncation, then the partial write,
/// then the finished file held still.
/// Expected: nothing at the empty state, nothing at the partial state, and the
/// finished state reported once it has been seen twice - so the empty file in
/// the middle is never handed to anyone.
#[test]
fn a_document_still_being_written_is_not_reported() {
    let mut watcher = watching(at(1, 400)).settle_after(2);

    assert_eq!(watcher.observe(at(2, 0)), None, "truncated");
    assert_eq!(watcher.observe(at(2, 180)), None, "partly written");
    assert_eq!(watcher.observe(at(2, 420)), None, "finished, seen once");
    assert_eq!(
        watcher.observe(at(2, 420)),
        Some(at(2, 420)),
        "and still there, so it is reported"
    );
}

/// TC-WATCH-3b: the settle count is the count that was asked for.
///
/// Written because a mutation survived: reporting on the *second* identical
/// observation passes every case built around a settle count of two, since
/// two is also the correct answer there. A caller polling fast enough to need
/// three has to get three, or the window in which a half-written file looks
/// stable is exactly the thing they asked to widen and did not.
///
/// Input: a settle count of three, and a change held still.
/// Expected: nothing on the first two identical observations, and the change
/// on the third.
#[test]
fn the_settle_count_is_the_count_that_was_asked_for() {
    let mut watcher = watching(at(1, 100)).settle_after(3);

    assert_eq!(watcher.observe(at(2, 200)), None, "seen once");
    assert_eq!(watcher.observe(at(2, 200)), None, "seen twice");
    assert_eq!(
        watcher.observe(at(2, 200)),
        Some(at(2, 200)),
        "three was what was asked for"
    );
}

/// TC-WATCH-4: a file that returns to what it was is not a change.
///
/// An editor that saves a buffer nobody edited, or a write that is undone
/// before the next poll, has produced nothing a reader needs to act on.
///
/// Input: a settle count of two, a change seen once, then the original state
/// back again.
/// Expected: nothing reported at any point, and a later real change still
/// reported - the pending state was abandoned, not remembered.
#[test]
fn a_file_that_returns_to_what_it_was_is_not_a_change() {
    let mut watcher = watching(at(1, 100)).settle_after(2);

    assert_eq!(watcher.observe(at(2, 100)), None, "seen once");
    assert_eq!(watcher.observe(at(1, 100)), None, "and undone");
    assert_eq!(watcher.observe(at(1, 100)), None, "still the original");

    assert_eq!(
        watcher.observe(at(3, 130)),
        None,
        "a real change, seen once"
    );
    assert_eq!(watcher.observe(at(3, 130)), Some(at(3, 130)), "and settled");
}

/// TC-WATCH-5: deletion and re-creation are changes like any other.
///
/// A deleted document is a real edit: it hands every key it set back to the
/// layer beneath it, which is behaviour `recompose` already has and this has
/// to be able to trigger. Absence has to be a state rather than an error, or
/// the watcher stops at the moment the user most wants it working.
///
/// Input: a present document deleted, then created again.
/// Expected: both reported, and the absent state carries `present: false`
/// rather than looking like an unchanged file.
#[test]
fn deletion_and_re_creation_are_changes() {
    let mut watcher = watching(at(1, 100));

    let gone = Stamp {
        present: false,
        len: 0,
        modified: None,
    };
    assert_eq!(watcher.observe(gone), Some(gone));
    assert_eq!(watcher.observe(gone), None, "still gone is not news");
    assert_eq!(
        watcher.observe(at(5, 90)),
        Some(at(5, 90)),
        "and back again"
    );
}

/// TC-WATCH-6: a change of content at the same size is still a change.
///
/// A settings edit that swaps one word for another of the same length -
/// `debug` for `error`, say - leaves the length identical. A watcher keyed on
/// size alone would never report it, and the failure would look like the
/// feature simply not working for some edits.
///
/// Input: a new modification time at an unchanged length.
/// Expected: reported.
#[test]
fn a_change_at_the_same_size_is_still_a_change() {
    let mut watcher = watching(at(1, 100));

    assert_eq!(watcher.observe(at(2, 100)), Some(at(2, 100)));
}

/// TC-WATCH-7: a real edit on disk is seen, and reloads what it changed.
///
/// The end to end of it, through the filesystem and through `recompose`,
/// because the previous cases all stipulate the stamps and something has to
/// check that a real write produces a different one.
///
/// Input: a document with one setting, a watcher on it, then the file
/// rewritten with a different value.
/// Expected: the watcher reports a change, and recomposing on that report
/// resolves the new value from the file layer.
#[test]
fn a_real_edit_is_seen_and_reloads_what_it_changed() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, json!({ "log": { "level": "info" } }).to_string()).expect("write");

    let mut config = Config::default();
    config.set("log.level", json!("warn"), Layer::Default);
    recompose(&mut config, &path).expect("first read");
    assert_eq!(
        config.get("log.level").expect("resolved").value,
        json!("info")
    );

    let mut watcher = Watcher::new(&path);
    assert_eq!(watcher.poll(), None, "nothing has happened yet");

    std::fs::write(
        &path,
        json!({ "log": { "level": "debug" }, "agent": { "max_steps": 3 } }).to_string(),
    )
    .expect("rewrite");

    assert!(watcher.poll().is_some(), "the rewrite is seen");
    let changed = recompose(&mut config, &path).expect("re-read");
    assert_eq!(
        config.get("log.level").expect("resolved").value,
        json!("debug")
    );
    assert_eq!(changed.changed, vec!["agent.max_steps", "log.level"]);
}

/// TC-WATCH-7b: a stamp of a real file carries its modification time.
///
/// Written because a mutation survived: dropping the modification time from
/// `Stamp::of` and keying on length alone passed every case, since the
/// stipulated-stamp cases never call it and the real-file case happened to
/// change the length too. An edit that swaps one word for another of the same
/// length - `info` for `warn` - would then never be noticed, and it would look
/// like reload simply not working for some edits.
///
/// Asserting the field is present is deliberate rather than asserting two
/// same-length writes differ: that would depend on the filesystem's timestamp
/// granularity, which is not this seam's business and is not the same
/// everywhere.
///
/// Input: a real file, stamped.
/// Expected: present, its true length, and a modification time that is
/// actually read.
#[test]
fn a_stamp_of_a_real_file_carries_its_modification_time() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{}").expect("write");

    let stamp = Stamp::of(&path);

    assert!(stamp.present);
    assert_eq!(stamp.len, 2);
    assert!(
        stamp.modified.is_some(),
        "a watcher keyed on length alone misses an edit that keeps the length"
    );

    let absent = Stamp::of(&dir.path().join("nothing.json"));
    assert!(!absent.present);
    assert_eq!(absent.modified, None);
}

/// TC-WATCH-8: a bad edit is reported, changes nothing, and the watcher keeps
/// working.
///
/// The failure this exists to prevent. `recompose` already leaves a running
/// configuration alone when a document will not parse; what would make that
/// useless is a watcher that stopped afterwards, because then one typo is
/// permanent until a restart - and the user's next action, fixing the typo,
/// would appear to do nothing.
///
/// Input: a good document, then an unparsable one, then a good one again.
/// Expected: the bad edit is reported by the watcher and refused by
/// `recompose` with the configuration untouched; the repair is then reported
/// and applied.
#[test]
fn a_bad_edit_leaves_the_configuration_alone_and_the_watcher_running() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, json!({ "log": { "level": "info" } }).to_string()).expect("write");

    let mut config = Config::default();
    recompose(&mut config, &path).expect("first read");
    let mut watcher = Watcher::new(&path);

    std::fs::write(&path, "{ this is not json").expect("bad write");
    assert!(watcher.poll().is_some(), "a bad edit is still an edit");
    assert!(
        recompose(&mut config, &path).is_err(),
        "and it is refused rather than applied"
    );
    assert_eq!(
        config.get("log.level").expect("still resolved").value,
        json!("info"),
        "the running configuration is what it was"
    );

    std::fs::write(&path, json!({ "log": { "level": "error" } }).to_string()).expect("repair");
    assert!(
        watcher.poll().is_some(),
        "the watcher did not stop at the bad edit"
    );
    recompose(&mut config, &path).expect("re-read");
    assert_eq!(
        config.get("log.level").expect("resolved").value,
        json!("error")
    );
}

/// TC-WATCH-9: a settle count below one is read as one.
///
/// Zero would mean "report before observing", which is not a state this can be
/// in. Clamping rather than refusing keeps the builder infallible for a value
/// that has an obvious intended meaning.
///
/// Expected: zero behaves as one, and the documented default interval is what
/// it says.
#[test]
fn a_settle_count_below_one_is_read_as_one() {
    let mut watcher = watching(at(1, 100)).settle_after(0);
    assert_eq!(watcher.observe(at(2, 100)), Some(at(2, 100)));

    assert_eq!(DEFAULT_POLL_INTERVAL, Duration::from_millis(500));
}

/// A watcher whose baseline is a stipulated state rather than a real file.
fn watching(baseline: Stamp) -> Watcher {
    let mut watcher = Watcher::new("/nonexistent/settings.json");
    // The constructor's baseline is "absent", which is a legitimate state; a
    // case that wants to start from a present document says so here rather
    // than creating a file to describe one.
    let _ = watcher.observe(baseline);
    watcher
}

/// A stamp, by modification time and length - the two things a poll can see.
fn at(seconds: u64, len: u64) -> Stamp {
    Stamp {
        present: true,
        len,
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
    }
}
