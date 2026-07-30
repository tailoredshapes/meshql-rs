//! Source-level guards.
//!
//! These are the tests that stop a *later* commit from undoing the design. Each
//! one exists because the property it defends is invisible at runtime on the
//! happy path: an adapter that scans is correct-but-ruinous, and a consumer with
//! `auto_commit: true` looks fine until a handler returns `Err`.
//!
//! They read `src/`, not `tests/`. Test code is allowed to construct the wrong
//! thing on purpose — `tests/consumer_offset_defects.rs` does exactly that to
//! demonstrate the defect — and a guard that forbade it would forbid the
//! demonstration.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn source_files() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(src_dir()).expect("src/ is readable") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&path).expect("readable source file");
            files.push((path, text));
        }
    }
    assert!(!files.is_empty(), "found no sources under {:?}", src_dir());
    files
}

/// Lines that are neither blank nor a `//` comment. Doc comments in this crate
/// quote the very constructs the guards forbid, so a naive substring search over
/// whole files would fire on its own explanation of the defect.
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with("//"))
        .collect()
}

/// `poll` advances the cursor to `tail` before the caller folds, `close` commits
/// when `auto_commit` is set, and `Drop` calls `close`. So `auto_commit: true`
/// plus an `Err` from a handler commits offsets for records that were never
/// folded — at-most-once, permanent, silent. There is no legitimate use of it.
#[test]
fn no_production_code_sets_auto_commit_true() {
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        for (number, line) in code_lines(&text) {
            let squashed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if squashed.contains("auto_commit:true") {
                offenders.push(format!("{}:{number}: {line}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "auto_commit: true commits offsets for records a failing handler never folded, \
         which loses them permanently and silently:\n{}",
        offenders.join("\n")
    );
}

/// `merk_object::consumer::Consumer` is the type carrying the defect above, and
/// it cannot express "one partition" anyway — `subscribe` takes topics. This
/// crate drives `Topic`/`Partition`/`ConsumerGroup` instead, and this guard is
/// what stops someone reaching for the convenient wrapper later.
#[test]
fn production_code_does_not_use_the_raw_consumer() {
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        for (number, line) in code_lines(&text) {
            if line.contains("consumer::Consumer") || line.contains("ConsumerConfig") {
                offenders.push(format!("{}:{number}: {line}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "merk_object::consumer::Consumer advances its cursor at poll time and commits on \
         Drop; use SafeConsumer:\n{}",
        offenders.join("\n")
    );
}

/// There must be no `Searcher` here at all. A `Searcher` over a log has to scan
/// it, and a search that works-but-slowly is how downloading the whole log per
/// GraphQL query becomes production behaviour by accident. Projections are read
/// from an indexed store.
#[test]
fn the_crate_implements_no_searcher() {
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        for (number, line) in code_lines(&text) {
            if line.contains("Searcher for") || line.contains("impl Searcher") {
                offenders.push(format!("{}:{number}: {line}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a Searcher over merk-cloud scans the topic from offset zero per query:\n{}",
        offenders.join("\n")
    );
}

/// The refusal is structural because `AppendOnlyLog` holds nothing that can
/// read: a `Producer` exposes only `send` and `send_batch`, and keeps its broker
/// private. Restoring a broker, topic or partition field is what a change would
/// have to do to make a scan possible, and this is the test that makes that show
/// up as a failure rather than as a performance mystery in production.
#[test]
fn the_append_only_log_holds_nothing_that_can_read() {
    let path = src_dir().join("log.rs");
    let text = std::fs::read_to_string(&path).expect("log.rs is readable");

    let start = text
        .find("pub struct AppendOnlyLog")
        .expect("AppendOnlyLog is declared in log.rs");
    let open = text[start..].find('{').expect("struct body opens") + start;
    let close = text[open..].find('}').expect("struct body closes") + open;
    let body = &text[open + 1..close];

    let fields: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .filter_map(|line| line.split(':').next().map(|name| name.trim().to_string()))
        .collect();

    assert_eq!(
        fields,
        vec!["producer".to_string(), "topic".to_string()],
        "AppendOnlyLog grew a field. If it is a broker, a topic handle or a \
         partition handle, the crate can now read the log and the whole reason it \
         exists is gone. Fields found: {fields:?}"
    );
}
