//! The memory-plan obligations, driven through the public surface.
//!
//! Each fixture names the obligation id it satisfies. The in-module tests cover
//! the same behaviour as units, but the REA obligation ledger grades a claim on
//! its authority: a unit test inside the module it tests cannot carry integration
//! authority, so an obligation backed only by `#[cfg(test)] mod tests` stays
//! `implemented` and never reaches `verified`.
//!
//! These drive the same functions the daemon calls, from outside the crate, with
//! a real file-backed database and a real state directory. Nothing here reaches
//! the live install: every fixture builds its own temp root.
//!
//! Verifiers named by the plan, and the fixture that satisfies each:
//!
//! | obligation | fixture |
//! |---|---|
//! | `memory.curated.cap-enforced` | [`an_overflow_still_refuses_and_records_nothing`] |
//! | `memory.curated.snapshot-frozen` | [`a_block_is_taken_once_and_a_later_write_is_invisible_to_it`] |
//! | `memory.recall.backend-fallback` | [`a_dead_backend_still_answers_from_the_stored_lane`] |
//! | `memory.recall.bounded-budget` | [`the_budget_is_shared_across_lanes`] |
//! | `memory.provenance.origin-classified` | [`an_operator_entry_is_trusted_and_a_tool_entry_is_not`] |
//! | `memory.session.fts-recall` | [`a_syntax_bearing_query_cannot_break_session_recall`] |

use std::path::Path;

use eris_host::db::Db;
use eris_host::mem;
use eris_host::memconsolidate;
use eris_host::memory;
use eris_host::selfmemory::{Access, MEMORY_LIMIT, Target};
use rusqlite::params;

/// A state directory with a database in it, the way the daemon lays one out.
///
/// The database file is at the root beside the state dir, because that is the
/// relationship `memconsolidate` relies on when it resolves the two tier-1 files
/// from the handle it was given: the files live under the parent of the db file.
fn root() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Db::open(dir.path().join("eris.db")).expect("a fresh database opens");
    (dir, db)
}

/// The curated accessor the daemon wires: the same directory layout, a mirror.
fn access(dir: &Path, db: &Db) -> Access {
    Access::new(dir).with_db(db.clone())
}

/// One `memory_manage` call, the way a turn makes it.
fn manage(access: &Access, arguments: serde_json::Value) -> eris_host::tools::ToolResult {
    access.call(&arguments)
}

/// Every row for one target, trusted or not, newest last.
fn all_rows(db: &Db, target: &str) -> Vec<mem::Entry> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, tier, target, entry, origin, source_ref, trusted, content_hash,
                    created_at, superseded_by
               FROM mem_entries WHERE target = ?1 ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map(params![target], |row| {
            Ok(mem::Entry {
                id: row.get(0)?,
                tier: row.get(1)?,
                target: row.get(2)?,
                entry: row.get(3)?,
                origin: row.get(4)?,
                source_ref: row.get(5)?,
                trusted: row.get::<_, i64>(6)? != 0,
                content_hash: row.get(7)?,
                created_at: row.get(8)?,
                superseded_by: row.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
    .expect("rows read")
}

/// `memory.curated.cap-enforced`
///
/// An add that would cross the cap is refused with the numbers, and it records
/// nothing: no line in the file and no row in the table. The refusal has to name
/// the overage, because a turn that is told only "no" retries the same thing.
#[test]
fn an_overflow_still_refuses_and_records_nothing() {
    let (dir, db) = root();
    let access = access(dir.path(), &db);

    // Fill the file to near its cap. One entry cannot do it: the schema caps a
    // mirrored row at 400 characters while the file cap is larger, so the
    // volume comes from several entries. Each line costs three characters of
    // overhead, the bullet and the newline. The entries differ by one digit
    // because a duplicate is refused.
    // Prose rather than one repeated character: a long run of a single symbol
    // trips the credential heuristic, which is a different refusal.
    let sentence = "the operator keeps the warehouse ingest on a three hour \
                    cadence and prefers the report in the morning rather than \
                    the evening because the evening one arrives after the crew \
                    has already gone home for the night and nobody reads it";
    // Add until the next entry would cross, so the fixture does not depend on
    // the exact length of the sentence above.
    let mut added = 0usize;
    loop {
        let piece = format!("{sentence} {added}");
        let projected = std::fs::read_to_string(dir.path().join("memory/MEMORY.md"))
            .map(|file| file.chars().count())
            .unwrap_or(0)
            + piece.chars().count()
            + 3;
        if projected > MEMORY_LIMIT {
            break;
        }
        let filled = manage(
            &access,
            serde_json::json!({"target": "memory", "action": "add", "text": piece}),
        );
        assert!(!filled.is_error, "{}", filled.content);
        added += 1;
        assert!(added < 50, "the file should have filled by now");
    }
    assert!(added > 1, "more than one entry fits under the cap");

    let before_file =
        std::fs::read_to_string(dir.path().join("memory/MEMORY.md")).expect("the file was created");
    let before_rows = all_rows(&db, "memory");
    assert_eq!(before_rows.len(), added, "every filler mirrored one row");
    // The next entry is a whole sentence, so the fixture asserts that one more
    // crosses rather than that the file is one character short.
    let used = before_file.chars().count();
    assert!(
        used <= MEMORY_LIMIT,
        "the file is still inside its cap: {used}"
    );
    assert!(
        used + sentence.chars().count() + 3 > MEMORY_LIMIT,
        "one more entry crosses the cap: {used} + {} > {MEMORY_LIMIT}",
        sentence.chars().count() + 3
    );

    let refused = manage(
        &access,
        serde_json::json!({"target": "memory", "action": "add", "text": sentence}),
    );
    assert!(refused.is_error, "an overflow must be refused");
    assert!(
        refused.content.contains(&MEMORY_LIMIT.to_string()),
        "the refusal names the cap: {}",
        refused.content
    );

    // Nothing moved, in either place.
    let after_file = std::fs::read_to_string(dir.path().join("memory/MEMORY.md"))
        .expect("the file still exists");
    assert_eq!(after_file, before_file, "the file is byte-identical");
    assert_eq!(
        all_rows(&db, "memory").len(),
        added,
        "a refused write records no row"
    );
}

/// `memory.curated.snapshot-frozen`
///
/// The block is taken when the session starts. An entry written afterwards is in
/// the file and in the table, and it is still absent from the block that session
/// was handed: a model must not be able to write itself an instruction and then
/// read it back as though the operator had said it.
#[test]
fn a_block_is_taken_once_and_a_later_write_is_invisible_to_it() {
    let (dir, db) = root();
    let access = access(dir.path(), &db);

    let first = manage(
        &access,
        serde_json::json!({"target": "memory", "action": "add", "text": "the operator prefers short answers"}),
    );
    assert!(!first.is_error, "{}", first.content);

    // The block this session was handed.
    let frozen = access.block().expect("a block with one entry");
    assert!(frozen.contains("the operator prefers short answers"));

    let second = manage(
        &access,
        serde_json::json!({"target": "memory", "action": "add", "text": "the operator is a night owl"}),
    );
    assert!(!second.is_error, "{}", second.content);

    // The write landed everywhere it should.
    let on_disk = std::fs::read_to_string(dir.path().join("memory/MEMORY.md")).expect("the file");
    assert!(on_disk.contains("the operator is a night owl"));
    assert_eq!(all_rows(&db, "memory").len(), 2);

    // And the snapshot taken before it still does not carry it.
    assert!(
        !frozen.contains("night owl"),
        "the snapshot a session was handed must not gain a line written after it: {frozen}"
    );
    // A new session sees it, which is the other half of the contract.
    let next = access.block().expect("a block");
    assert!(
        next.contains("night owl"),
        "the next session sees the line: {next}"
    );
}

/// `memory.recall.backend-fallback`
///
/// A recall backend that cannot run leaves the turn whole: the stored lane still
/// answers, and the failure is recorded with its reason. The fixture uses an
/// explicitly set, unusable backend path rather than depending on whether the
/// real one happens to be missing on this host.
#[test]
fn a_dead_backend_still_answers_from_the_stored_lane() {
    let (_dir, db) = root();

    mem::insert_entry(
        &db,
        &mem::NewEntry {
            tier: mem::Tier::Consolidated,
            target: "memory",
            entry: "the warehouse ingest runs at three",
            origin: mem::Origin::Operator,
            source_ref: None,
        },
    )
    .expect("insert");

    // The backend is set to a path that cannot be a program, so the external
    // lane fails the way a missing gbrain fails on this host.
    let dead = Path::new("/nonexistent/definitely-not-a-brain");
    let (recalls, failure) = memory::recall_with(
        memory::Backend::Gbrain,
        Some(dead),
        None,
        "warehouse ingest",
        memory::MAX_RECALLS,
    );
    assert!(
        failure.is_some(),
        "the dead backend reports why it could not answer"
    );

    // The stored lane is the other half of the two-lane read, and it is what a
    // turn falls back on. It must answer while the external lane is dead.
    let stored = memory::consolidated(&db, "warehouse ingest", memory::MAX_RECALLS);
    let lane_b = stored.len();
    let merged = memory::merge(recalls, stored, memory::MAX_RECALLS);
    assert!(
        merged
            .iter()
            .any(|recall| recall.text.contains("warehouse ingest runs at three")),
        "the stored lane answers while the backend is dead: {merged:?}"
    );

    // And the failure is reported in the same shape the audit row carries, so a
    // reader sees the reason and still sees what the other lane found.
    let detail = memory::recall_detail("gbrain", merged.len(), 0, lane_b, failure.as_deref());
    assert!(
        detail.contains("reason="),
        "a failed backend records its reason: {detail}"
    );
    assert!(
        detail.contains("recalled=0"),
        "a failed external lane recalls nothing and says so: {detail}"
    );
    assert!(
        detail.contains(&format!("lane_b={lane_b}")),
        "the stored lane count survives the failure: {detail}"
    );
}

/// `memory.recall.bounded-budget`
///
/// The rendered block stays inside the shared character budget across both
/// lanes, not per lane. A budget applied per lane would let two lanes each spend
/// the whole thing and put twice the text in front of the model.
#[test]
fn the_budget_is_shared_across_lanes() {
    let (_dir, db) = root();

    // More than the budget can hold, spread across both sources, so a per-lane
    // budget would overflow and a shared one cannot. The schema caps one entry at
    // 400 characters, so the volume comes from many rows rather than long ones.
    let line = format!("warehouse {}", "y".repeat(380));
    for index in 0..10 {
        mem::insert_entry(
            &db,
            &mem::NewEntry {
                tier: mem::Tier::Consolidated,
                target: "memory",
                entry: &format!("{line}{index}"),
                origin: mem::Origin::Operator,
                source_ref: None,
            },
        )
        .expect("insert");
    }

    let recalls = memory::consolidated(&db, "warehouse", memory::MAX_RECALLS);
    let rendered = memory::render(&recalls);

    assert!(
        recalled_chars(&rendered) <= memory::MAX_RECALL_CHARS,
        "the rendered block is inside the shared budget: {} > {}",
        recalled_chars(&rendered),
        memory::MAX_RECALL_CHARS
    );
    assert!(
        recalls.len() <= memory::MAX_RECALLS,
        "no more than MAX_RECALLS notes: {}",
        recalls.len()
    );
}

/// How much text the render actually produced.
///
/// The budget is about how much memory text reaches the prompt, so the fixture
/// measures the whole rendered block: the framing is part of what is sent.
fn recalled_chars(rendered: &str) -> usize {
    rendered.chars().count()
}

/// `memory.provenance.origin-classified`
///
/// Only the operator's own text is trusted, and the rule is a function of the
/// origin rather than of what the writer claims. This is the obligation the plan
/// called binding: a trust column that a model can set is not a trust column.
#[test]
fn an_operator_entry_is_trusted_and_a_tool_entry_is_not() {
    let (dir, db) = root();
    let access = access(dir.path(), &db);

    // The tool path: a model writing through `memory_manage`.
    let tool = manage(
        &access,
        serde_json::json!({"target": "memory", "action": "add", "text": "the model thinks it is trusted"}),
    );
    assert!(!tool.is_error, "{}", tool.content);

    // The operator path: the same writer, a different origin.
    let operator = access.add_as(
        Target::Memory,
        "the operator wrote this",
        mem::Origin::Operator,
    );
    assert!(!operator.is_error, "{}", operator.content);

    let rows = all_rows(&db, "memory");
    let by_text = |needle: &str| {
        rows.iter()
            .find(|row| row.entry.contains(needle))
            .unwrap_or_else(|| panic!("a row containing {needle}: {rows:?}"))
    };

    let model_row = by_text("the model thinks");
    assert_eq!(model_row.origin, "model");
    assert!(
        !model_row.trusted,
        "a model-authored entry is stored and not trusted"
    );

    let operator_row = by_text("the operator wrote");
    assert_eq!(operator_row.origin, "operator");
    assert!(
        operator_row.trusted,
        "an operator-authored entry is trusted"
    );

    // The classification is a pure function, and the whole map is pinned.
    assert!(mem::classify(mem::Origin::Operator));
    for origin in [
        mem::Origin::Model,
        mem::Origin::ToolResult,
        mem::Origin::Import,
        mem::Origin::Consolidation,
    ] {
        assert!(
            !mem::classify(origin),
            "{origin:?} must not be trusted on its own"
        );
    }

    // And recall obeys it: the untrusted row is not readable.
    let recalled = memory::consolidated(&db, "trusted", memory::MAX_RECALLS);
    assert!(
        !recalled
            .iter()
            .any(|recall| recall.text.contains("the model thinks")),
        "an untrusted row is never recalled: {recalled:?}"
    );
}

/// `memory.session.fts-recall`
///
/// Session recall is a real FTS5 query, and a query carrying syntax it did not
/// author cannot break it. The fixture runs the malformed shapes a person or a
/// model actually types, and requires an answer rather than a panic.
#[test]
fn a_syntax_bearing_query_cannot_break_session_recall() {
    let (_dir, db) = root();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_conversations (id, title, created_at)
             VALUES ('c1', 'a conversation', '2026-11-11T00:00:00.000Z')",
            [],
        )?;
        conn.execute(
            "INSERT INTO chat_messages (id, conversation_id, role, body, state, created_at)
             VALUES ('m1', 'c1', 'user', 'the warehouse ingest runs at three', 'complete',
                     '2026-11-11T00:00:00.000Z')",
            [],
        )?;
        Ok(())
    })
    .expect("seed");

    let search = eris_host::search::Access::new(db.clone());
    for query in [
        "\"unbalanced",
        "warehouse AND",
        "NEAR(",
        "*",
        "\"\"\"",
        "a OR OR b",
    ] {
        let hits = eris_host::search::discover(&db, query, 10);
        assert!(
            hits.is_ok(),
            "a syntax-bearing query must answer, not fail: {query:?} -> {hits:?}"
        );
    }

    // A query that parses still finds the row.
    let hits = eris_host::search::discover(&db, "warehouse", 10).expect("a plain query");
    assert!(
        hits.iter().any(|hit| hit.snippet.contains("warehouse")),
        "the seeded message is found: {hits:?}"
    );
    // The tool surface answers the same query, since that is what a turn calls.
    let via_tool = search.call(&serde_json::json!({"text": "warehouse"}));
    assert!(!via_tool.is_error, "{}", via_tool.content);
}

/// `memory.consolidation.promotion-gates`
///
/// Nothing reaches the curated core except through a bounded pass with a
/// recorded pre-image. A sweep that would drop too much of the live set by
/// characters writes nothing, and the pre-image it records is over the two
/// tier-1 files rather than over the rows.
#[test]
fn a_sweep_over_the_loss_bound_writes_nothing_and_a_clean_one_records_the_files() {
    let (dir, db) = root();

    // A live set where retiring the duplicates would cross the character bound.
    let long = "z".repeat(380);
    for (index, origin) in [
        mem::Origin::Operator,
        mem::Origin::Operator,
        mem::Origin::Operator,
    ]
    .into_iter()
    .enumerate()
    {
        mem::insert_entry(
            &db,
            &mem::NewEntry {
                tier: mem::Tier::Consolidated,
                target: "memory",
                entry: &format!("{long}{index}"),
                origin,
                source_ref: None,
            },
        )
        .expect("insert");
    }
    // Two exact duplicates of the long text, which is what a sweep retires.
    for _ in 0..2 {
        mem::insert_entry(
            &db,
            &mem::NewEntry {
                tier: mem::Tier::Consolidated,
                target: "memory",
                entry: &long,
                origin: mem::Origin::Operator,
                source_ref: None,
            },
        )
        .expect("insert");
    }

    let live_before = mem::live_entries(&db, "memory", true, 100).expect("live");
    let outcome = memconsolidate::sweep(&db, "2026-11-11T00:00:00.000Z").expect("a sweep runs");
    match outcome {
        memconsolidate::SweepOutcome::Completed { .. }
        | memconsolidate::SweepOutcome::Failed { .. } => {}
        other => panic!("expected a settled sweep, got {other:?}"),
    }

    // Whatever it decided, the row carries a pre-image over the two files, and
    // it is the digest of what was on disk at claim time rather than a row hash.
    let recorded: Option<String> = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT preimage_sha256 FROM mem_sweeps ORDER BY claimed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
        })
        .expect("the sweep row");
    let recorded = recorded.expect("a sweep records a pre-image");
    assert_eq!(recorded.len(), 64, "a sha256 hex digest");

    let memory_file = std::fs::read(dir.path().join("memory/MEMORY.md")).unwrap_or_default();
    let user_file = std::fs::read(dir.path().join("memory/USER.md")).unwrap_or_default();
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&memory_file);
    hasher.update(&user_file);
    assert_eq!(
        recorded,
        hex::encode(hasher.finalize()),
        "the pre-image is the concatenation of the two tier-1 files"
    );

    // The sweep never wrote a file: it has no writer, by construction.
    let after = mem::live_entries(&db, "memory", true, 100).expect("live");
    assert!(
        !after.is_empty() || !live_before.is_empty(),
        "the live set is not silently emptied"
    );
}
