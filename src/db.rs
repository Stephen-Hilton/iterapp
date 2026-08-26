//! Durable state in SQLite (features/sqlite_storage.md).
//!
//! One database per project, `.iter/.engine/iter.db`, replacing the jsonl files
//! that used to hold the queue. Three forces drove the move:
//!
//! - **The webapp slowed as the queue grew.** Every request parsed the whole
//!   file and shipped every item, open and archived, full text included — a
//!   curve that only bends one way. Counts and summaries are now queries.
//! - **The open/closed SPLIT was itself a bug source.** An archived item lived
//!   in a different file, so every action verb that looked only in the open one
//!   answered "no such open work item" — retry offered a button that could not
//!   work, and delete was never implemented at all. One table with an
//!   `archived` flag makes those uniform row operations.
//! - **Valuable data lived in a directory named temp.** Critique documents —
//!   the record of what a reviewer caught before an item shipped — sat in
//!   `.iter/temp/` where a TTL sweeper would eventually eat them. The rule this
//!   schema enforces: nothing worth keeping lives where a cleanup policy can
//!   reach it.
//!
//! WAL mode, because the engine, the webapp and the CLI are three processes
//! reading and writing the same database: WAL lets readers run while a writer
//! commits, and `busy_timeout` makes a contended write wait rather than fail.
//! That is what retires the hand-rolled `.lock` sentinel files.
//!
//! **`archived` is a column, not a state.** A `failed` item is still OPEN while
//! it waits out its retry backoff — deriving "archived" from the state string
//! would resurrect items mid-retry, so the flag is explicit and the two are
//! kept independent.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::workitems::WorkItem;

pub fn db_path(project_root: &Path) -> PathBuf {
    crate::config::engine_dir(project_root).join("iter.db")
}

/// Open (creating if needed) the project database with the pragmas every
/// connection in every process needs. `busy_timeout` is generous because the
/// engine holds brief write transactions while several webapp requests may be
/// arriving; waiting is always better than surfacing a spurious failure.
pub fn open(project_root: &Path) -> rusqlite::Result<Connection> {
    let path = db_path(project_root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Hot columns are real columns so the engine can index and sort on them; the
/// rest of the work item rides in `body` as JSON, which SQLite queries natively
/// when something rarely-used is needed. `seq` preserves insertion order, which
/// the queue's file form gave for free and several callers still rely on.
pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS workitems (
            workid     TEXT PRIMARY KEY,
            seq        INTEGER,
            archived   INTEGER NOT NULL DEFAULT 0,
            state      TEXT    NOT NULL,
            item_type  TEXT    NOT NULL,
            title      TEXT    NOT NULL DEFAULT '',
            priority   INTEGER NOT NULL DEFAULT 5,
            source     TEXT    NOT NULL DEFAULT '',
            created_by TEXT    NOT NULL DEFAULT '',
            added      TEXT    NOT NULL DEFAULT '',
            closed     TEXT    NOT NULL DEFAULT '',
            body       TEXT    NOT NULL
        );
        CREATE INDEX IF NOT EXISTS workitems_archived_state ON workitems(archived, state);
        CREATE INDEX IF NOT EXISTS workitems_created_by     ON workitems(created_by);
        CREATE INDEX IF NOT EXISTS workitems_closed         ON workitems(closed);
        CREATE INDEX IF NOT EXISTS workitems_seq            ON workitems(seq);

        -- Every critical-review round, written by `iter critreview` itself at the
        -- moment it runs — the engine-owned write path, so agents never handle
        -- storage. Kept so "how often does the critic catch something real, per
        -- agent type, per month" is a query rather than an archaeology dig
        -- through whichever temp files happened to survive.
        CREATE TABLE IF NOT EXISTS critiques (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            workid      TEXT NOT NULL DEFAULT '',
            round       INTEGER NOT NULL DEFAULT 1,
            persona     TEXT NOT NULL DEFAULT '',
            agent_type  TEXT NOT NULL DEFAULT '',
            material    TEXT NOT NULL DEFAULT '',
            critique    TEXT NOT NULL DEFAULT '',
            disposition TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS critiques_workid ON critiques(workid);

        -- Questions and their answers (features/Question_state.md). The pair also
        -- lives on the work item, which is what the agent reads; this table is
        -- what makes "how long did each decision wait on a human" answerable.
        CREATE TABLE IF NOT EXISTS questions (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            workid      TEXT NOT NULL DEFAULT '',
            question    TEXT NOT NULL DEFAULT '',
            answer      TEXT NOT NULL DEFAULT '',
            asked_at    TEXT NOT NULL DEFAULT '',
            answered_at TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS questions_workid ON questions(workid);

        CREATE TABLE IF NOT EXISTS spend (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            ts            TEXT NOT NULL DEFAULT '',
            workid        TEXT NOT NULL DEFAULT '',
            agent         TEXT NOT NULL DEFAULT '',
            turn          TEXT NOT NULL DEFAULT '',
            usd           REAL NOT NULL DEFAULT 0,
            input_tokens  INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS spend_ts ON spend(ts);

        CREATE TABLE IF NOT EXISTS sched_log (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            ts     TEXT NOT NULL DEFAULT '',
            entry  TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
}

/// Rehydrate a work item from its stored JSON. A row whose body will not parse
/// is skipped rather than fatal — the same tolerance `Queue::load` gave a
/// malformed jsonl line, so one bad row can never take the queue down.
fn row_to_item(body: &str) -> Option<WorkItem> {
    serde_json::from_str::<WorkItem>(body).ok().map(|mut i| {
        i.normalize_codepaths();
        i
    })
}

/// Every item on one side of the archive line, in insertion order.
pub fn load_items(conn: &Connection, archived: bool) -> Vec<WorkItem> {
    let mut stmt = match conn
        .prepare("SELECT body FROM workitems WHERE archived = ?1 ORDER BY seq, rowid")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(params![archived as i64], |r| r.get::<_, String>(0));
    match rows {
        Ok(rows) => rows.flatten().filter_map(|b| row_to_item(&b)).collect(),
        Err(_) => Vec::new(),
    }
}

/// One item by id, whichever side of the archive it is on.
pub fn get_item(conn: &Connection, workid: &str) -> Option<(WorkItem, bool)> {
    conn.query_row(
        "SELECT body, archived FROM workitems WHERE workid = ?1",
        params![workid],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|(body, arch)| row_to_item(&body).map(|i| (i, arch == 0)))
}

/// `state -> count`, plus the `open`/`total` rollups, computed in SQLite. This
/// is the query that replaces parsing every item to count them.
pub fn counts(conn: &Connection) -> std::collections::HashMap<String, i64> {
    let mut out = std::collections::HashMap::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT state, archived, COUNT(*) FROM workitems GROUP BY state, archived")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        }) {
            let (mut open, mut total) = (0, 0);
            for (state, archived, n) in rows.flatten() {
                *out.entry(state).or_insert(0) += n;
                total += n;
                if archived == 0 {
                    open += n;
                }
            }
            out.insert("open".into(), open);
            out.insert("total".into(), total);
        }
    }
    out
}

/// A page of the archive, NEWEST first. The list view shows a window, not the
/// whole history — at 5,000 items shipping all of it is the cost the move to
/// SQLite exists to remove.
pub fn closed_page(conn: &Connection, offset: i64, limit: i64) -> Vec<WorkItem> {
    let mut stmt = match conn.prepare(
        "SELECT body FROM workitems WHERE archived = 1
         ORDER BY closed DESC, seq DESC LIMIT ?1 OFFSET ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(params![limit, offset], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows.flatten().filter_map(|b| row_to_item(&b)).collect(),
        Err(_) => Vec::new(),
    }
}

/// Just the ids on the archived side. The change-detector needs the archive's
/// id SET to tell "this row got archived" from "this row got deleted", and
/// nothing else about those rows — loading their bodies to answer a set-
/// membership question was the last place a hot path still paid for the whole
/// archive.
pub fn archived_ids(conn: &Connection) -> std::collections::HashSet<String> {
    let Ok(mut stmt) = conn.prepare("SELECT workid FROM workitems WHERE archived = 1") else {
        return std::collections::HashSet::new();
    };
    match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// Write one item, inserting or replacing in place. `seq` is preserved for an
/// item that already exists, so an update never reorders the queue.
pub fn put_item(conn: &Connection, item: &WorkItem, archived: bool) -> rusqlite::Result<()> {
    let body = serde_json::to_string(item).expect("workitem serializes");
    conn.execute(
        "INSERT INTO workitems
           (workid, seq, archived, state, item_type, title, priority, source, created_by, added, closed, body)
         VALUES (?1,
                 COALESCE((SELECT seq FROM workitems WHERE workid = ?1),
                          (SELECT IFNULL(MAX(seq), 0) + 1 FROM workitems)),
                 ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(workid) DO UPDATE SET
           archived = excluded.archived, state = excluded.state, item_type = excluded.item_type,
           title = excluded.title, priority = excluded.priority, source = excluded.source,
           created_by = excluded.created_by, added = excluded.added, closed = excluded.closed,
           body = excluded.body",
        params![
            item.workid,
            archived as i64,
            item.state,
            item.item_type,
            item.title,
            item.priority,
            item.source,
            item.created_by,
            item.times.added,
            item.times.closed,
            body,
        ],
    )?;
    Ok(())
}

pub fn delete_item(conn: &Connection, workid: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM workitems WHERE workid = ?1", params![workid])? > 0)
}

/// Replace the whole OPEN set with `items`, inside one transaction: the
/// database equivalent of the old rewrite-the-file-under-a-lock protocol, which
/// several callers use to add, mutate and remove in a single atomic step.
/// Archived rows are untouched.
pub fn replace_open(conn: &mut Connection, items: &[WorkItem]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let keep: Vec<String> = items.iter().map(|i| i.workid.clone()).collect();
    {
        let mut stmt = tx.prepare("SELECT workid FROM workitems WHERE archived = 0")?;
        let existing: Vec<String> =
            stmt.query_map([], |r| r.get::<_, String>(0))?.flatten().collect();
        for gone in existing.iter().filter(|id| !keep.contains(id)) {
            tx.execute("DELETE FROM workitems WHERE workid = ?1", params![gone])?;
        }
    }
    for item in items {
        let body = serde_json::to_string(item).expect("workitem serializes");
        tx.execute(
            "INSERT INTO workitems
               (workid, seq, archived, state, item_type, title, priority, source, created_by, added, closed, body)
             VALUES (?1,
                     COALESCE((SELECT seq FROM workitems WHERE workid = ?1),
                              (SELECT IFNULL(MAX(seq), 0) + 1 FROM workitems)),
                     0, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(workid) DO UPDATE SET
               archived = 0, state = excluded.state, item_type = excluded.item_type,
               title = excluded.title, priority = excluded.priority, source = excluded.source,
               created_by = excluded.created_by, added = excluded.added,
               closed = excluded.closed, body = excluded.body",
            params![
                item.workid, item.state, item.item_type, item.title, item.priority,
                item.source, item.created_by, item.times.added, item.times.closed, body,
            ],
        )?;
    }
    tx.commit()
}

/* ------------------------------------------------- questions & critiques */

/// Log a question at the moment an item enters `question` state. The pair also
/// lives on the work item — that is what the agent reads — so this row exists
/// purely to make "how long did this decision wait on a human" a query.
///
/// Idempotent on the newest still-unanswered row: `iter ask` and a subsequent
/// mutate of the same item both run through here, and re-asking the identical
/// text should not manufacture a second wait to measure. A genuinely NEW
/// question (different text, or asked again after the last one was answered)
/// does get its own row, because that is a second wait.
pub fn record_question(conn: &Connection, workid: &str, question: &str, asked_at: &str) {
    let duplicate: bool = conn
        .query_row(
            "SELECT question FROM questions WHERE workid = ?1 AND answer = ''
             ORDER BY id DESC LIMIT 1",
            params![workid],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(|prev| prev == question)
        .unwrap_or(false);
    if duplicate {
        return;
    }
    let _ = conn.execute(
        "INSERT INTO questions (workid, question, asked_at) VALUES (?1, ?2, ?3)",
        params![workid, question, asked_at],
    );
}

/// Close the loop on the oldest question this item is still waiting on. The
/// answer is applied by the SERVER, so this is the one storage call that path
/// makes; it is deliberately infallible-looking for the same reason the rest of
/// this module tolerates a bad row — a bookkeeping failure must never block the
/// human's answer from reaching the item.
///
/// FIFO (`ORDER BY id`) rather than newest-first: if two questions somehow
/// stacked up, the one that has been waiting longest is the one being answered.
/// An answer arriving with no open question still gets a row, so the record of
/// what the human said survives even when the ask went unlogged.
pub fn record_answer(conn: &Connection, workid: &str, answer: &str, answered_at: &str) {
    let updated = conn
        .execute(
            "UPDATE questions SET answer = ?2, answered_at = ?3
             WHERE id = (SELECT id FROM questions WHERE workid = ?1 AND answer = ''
                         ORDER BY id LIMIT 1)",
            params![workid, answer, answered_at],
        )
        .unwrap_or(0);
    if updated == 0 {
        let _ = conn.execute(
            "INSERT INTO questions (workid, question, answer, asked_at, answered_at)
             VALUES (?1, '', ?2, '', ?3)",
            params![workid, answer, answered_at],
        );
    }
}

/// One critical-review round, written by `iter critreview` as it runs. `round`
/// is caller-supplied so a re-review of the same item stacks rather than
/// overwrites; `disposition` starts empty and is filled in later by
/// [`set_critique_disposition`] when the consuming agent reports what it did.
pub fn record_critique(
    conn: &Connection,
    workid: &str,
    round: i64,
    persona: &str,
    agent_type: &str,
    material: &str,
    critique: &str,
    created_at: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO critiques (workid, round, persona, agent_type, material, critique, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![workid, round, persona, agent_type, material, critique, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record what the reviewed agent DID with a round's feedback. Without this the
/// table answers "how often did the critic speak", which nobody asked; the
/// question is "how often did it catch something real", and only the consuming
/// agent knows that.
///
/// `round = None` targets the item's latest round, which is what the agent
/// coming straight out of a review means when it does not say.
pub fn set_critique_disposition(
    conn: &Connection,
    workid: &str,
    round: Option<i64>,
    disposition: &str,
) -> rusqlite::Result<bool> {
    let n = match round {
        Some(r) => conn.execute(
            "UPDATE critiques SET disposition = ?3 WHERE workid = ?1 AND round = ?2",
            params![workid, r, disposition],
        )?,
        None => conn.execute(
            "UPDATE critiques SET disposition = ?2
             WHERE id = (SELECT id FROM critiques WHERE workid = ?1 ORDER BY round DESC, id DESC LIMIT 1)",
            params![workid, disposition],
        )?,
    };
    Ok(n > 0)
}

/// The round number a new critique should take for this item: one past the
/// highest already stored, so `iter critreview` never has to be told where it is
/// in a multi-round review.
pub fn next_critique_round(conn: &Connection, workid: &str) -> i64 {
    conn.query_row(
        "SELECT IFNULL(MAX(round), 0) + 1 FROM critiques WHERE workid = ?1",
        params![workid],
        |r| r.get(0),
    )
    .unwrap_or(1)
}

/* ------------------------------------------------------ spend & sched_log */

/// Append one agent turn's receipts. Same append-only stream `spend.jsonl` was,
/// with the day rollup now a query instead of a whole-file parse.
pub fn record_spend(
    conn: &Connection,
    ts: &str,
    workid: &str,
    agent: &str,
    turn: &str,
    usd: f64,
    input_tokens: u64,
    output_tokens: u64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO spend (ts, workid, agent, turn, usd, input_tokens, output_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![ts, workid, agent, turn, usd, input_tokens as i64, output_tokens as i64],
    )?;
    Ok(())
}

/// USD spent on one day, by ISO date prefix. `ts LIKE '<day>%'` uses the
/// `spend_ts` index, so the budget check the scheduler runs before every
/// dispatch stops scaling with the length of the ledger.
pub fn spend_for_day(conn: &Connection, day_prefix: &str) -> f64 {
    conn.query_row(
        "SELECT IFNULL(SUM(usd), 0) FROM spend WHERE ts LIKE ?1 || '%'",
        params![day_prefix],
        |r| r.get(0),
    )
    .unwrap_or(0.0)
}

/// Append one scheduler audit line. `entry` is the whole JSON record the file
/// form held on one line, kept verbatim so `iter export --table sched_log`
/// reproduces the retired file byte for byte; `ts` is lifted out of it into its
/// own column purely so the log can be ordered and windowed in SQL.
pub fn record_sched(conn: &Connection, ts: &str, entry: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO sched_log (ts, entry) VALUES (?1, ?2)", params![ts, entry])?;
    Ok(())
}

/// The `ts` field of a jsonl audit line, for the column that indexes it.
fn ts_of(line: &str) -> String {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("ts").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .unwrap_or_default()
}

/// One-time migration of `spend.jsonl`, on the same terms as [`import_jsonl`]:
/// only when the table is empty, verified by count before the file is retired,
/// and the file renamed rather than deleted. Returns rows inserted.
///
/// Gated on the file EXISTING rather than on a table count, because the callers
/// are `spend::record` and `spend::today_usd` — hot paths that must not pay for
/// a query on every turn. After the rename the gate is a failed `stat`.
pub fn import_spend_jsonl(conn: &mut Connection, project_root: &Path) -> rusqlite::Result<usize> {
    let path = crate::config::engine_dir(project_root).join("spend.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let already: i64 =
        conn.query_row("SELECT COUNT(*) FROM spend", [], |r| r.get(0)).unwrap_or(0);
    if already > 0 {
        return Ok(0);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let rows: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let tx = conn.transaction()?;
    for row in &rows {
        let s = |k: &str| row.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let n = |k: &str| row.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        tx.execute(
            "INSERT INTO spend (ts, workid, agent, turn, usd, input_tokens, output_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                s("ts"),
                s("workid"),
                s("agent"),
                s("turn"),
                row.get("usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
                n("input_tokens"),
                n("output_tokens"),
            ],
        )?;
    }
    tx.commit()?;

    let imported: i64 =
        conn.query_row("SELECT COUNT(*) FROM spend", [], |r| r.get(0)).unwrap_or(0);
    if imported >= rows.len() as i64 {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.imported"));
    }
    Ok(rows.len())
}

/// One-time migration of `sched_log.jsonl`, identical in shape to
/// [`import_spend_jsonl`]. Each line goes into `entry` unchanged — the audit
/// trail's value is that it is exactly what the engine wrote.
pub fn import_sched_log_jsonl(conn: &mut Connection, project_root: &Path) -> rusqlite::Result<usize> {
    let path = crate::config::engine_dir(project_root).join("sched_log.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let already: i64 =
        conn.query_row("SELECT COUNT(*) FROM sched_log", [], |r| r.get(0)).unwrap_or(0);
    if already > 0 {
        return Ok(0);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();

    let tx = conn.transaction()?;
    for line in &lines {
        tx.execute(
            "INSERT INTO sched_log (ts, entry) VALUES (?1, ?2)",
            params![ts_of(line), line],
        )?;
    }
    tx.commit()?;

    let imported: i64 =
        conn.query_row("SELECT COUNT(*) FROM sched_log", [], |r| r.get(0)).unwrap_or(0);
    if imported >= lines.len() as i64 {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.imported"));
    }
    Ok(lines.len())
}

/* ------------------------------------------------------------ export */

/// The tables `iter export` will dump, in the order the error message lists them.
pub const EXPORTABLE: &[&str] = &["workitems", "critiques", "questions", "spend", "sched_log"];

/// Which side of the archive line an export covers.
pub enum ExportScope {
    All,
    Open,
    Archived,
}

/// One table as jsonl lines — the escape hatch for the one real loss in the
/// move to SQLite: the queue stopped being a text file that a human could grep
/// and a git diff could show.
///
/// `workitems` lines are the stored `body` VERBATIM, so an exported line is a
/// valid work item that `iter add --file` accepts — a queue dumped and re-added
/// survives the round trip. The other tables are rendered from their columns,
/// except `sched_log`, whose `entry` already IS the line the file form held.
pub fn export_table(conn: &Connection, table: &str, scope: ExportScope) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    match table {
        "workitems" => {
            let sql = match scope {
                ExportScope::All => "SELECT body FROM workitems ORDER BY seq, rowid",
                ExportScope::Open => "SELECT body FROM workitems WHERE archived = 0 ORDER BY seq, rowid",
                ExportScope::Archived => "SELECT body FROM workitems WHERE archived = 1 ORDER BY seq, rowid",
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            out.extend(rows.flatten());
        }
        "critiques" => {
            let mut stmt = conn.prepare(
                "SELECT id, workid, round, persona, agent_type, material, critique, disposition, created_at
                 FROM critiques ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "workid": r.get::<_, String>(1)?,
                    "round": r.get::<_, i64>(2)?,
                    "persona": r.get::<_, String>(3)?,
                    "agent_type": r.get::<_, String>(4)?,
                    "material": r.get::<_, String>(5)?,
                    "critique": r.get::<_, String>(6)?,
                    "disposition": r.get::<_, String>(7)?,
                    "created_at": r.get::<_, String>(8)?,
                })
                .to_string())
            })?;
            out.extend(rows.flatten());
        }
        "questions" => {
            let mut stmt = conn.prepare(
                "SELECT id, workid, question, answer, asked_at, answered_at FROM questions ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "workid": r.get::<_, String>(1)?,
                    "question": r.get::<_, String>(2)?,
                    "answer": r.get::<_, String>(3)?,
                    "asked_at": r.get::<_, String>(4)?,
                    "answered_at": r.get::<_, String>(5)?,
                })
                .to_string())
            })?;
            out.extend(rows.flatten());
        }
        "spend" => {
            let mut stmt = conn.prepare(
                "SELECT ts, workid, agent, turn, usd, input_tokens, output_tokens FROM spend ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(serde_json::json!({
                    "ts": r.get::<_, String>(0)?,
                    "workid": r.get::<_, String>(1)?,
                    "agent": r.get::<_, String>(2)?,
                    "turn": r.get::<_, String>(3)?,
                    "usd": r.get::<_, f64>(4)?,
                    "input_tokens": r.get::<_, i64>(5)?,
                    "output_tokens": r.get::<_, i64>(6)?,
                })
                .to_string())
            })?;
            out.extend(rows.flatten());
        }
        "sched_log" => {
            let mut stmt = conn.prepare("SELECT ts, entry FROM sched_log ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for (ts, entry) in rows.flatten() {
                // The engine writes a JSON object per fire, so the entry is
                // already the exported line. Anything else gets wrapped rather
                // than emitted raw, so every line of the dump is valid jsonl.
                if serde_json::from_str::<serde_json::Value>(&entry).is_ok() {
                    out.push(entry);
                } else {
                    out.push(serde_json::json!({ "ts": ts, "entry": entry }).to_string());
                }
            }
        }
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unknown table \"{}\"",
                table
            )))
        }
    }
    Ok(out)
}

/// Row count of one exportable table, for `iter status` and the export's own
/// progress line. Returns 0 for an unknown table rather than erroring — every
/// caller already validated the name against [`EXPORTABLE`].
pub fn table_count(conn: &Connection, table: &str) -> i64 {
    if !EXPORTABLE.contains(&table) {
        return 0;
    }
    // The table name cannot be a bound parameter, hence the format!; the
    // EXPORTABLE check above is what keeps that from being an injection.
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0)).unwrap_or(0)
}

/* ------------------------------------------------------------ import */

/// One-time migration from the jsonl files. Returns (open, closed) row counts
/// actually inserted.
///
/// Runs only when the database has no work items at all, so it is safe to call
/// on every startup: an already-migrated project skips it, and a project that
/// never had jsonl files imports nothing. The files are RENAMED rather than
/// deleted — the escape hatch stays on disk until a clean week has passed.
pub fn import_jsonl(conn: &mut Connection, project_root: &Path) -> rusqlite::Result<(usize, usize)> {
    let already: i64 =
        conn.query_row("SELECT COUNT(*) FROM workitems", [], |r| r.get(0)).unwrap_or(0);
    if already > 0 {
        return Ok((0, 0));
    }
    let dir = crate::config::engine_dir(project_root);
    let read = |name: &str| -> Vec<WorkItem> {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .map(|mut i| {
                i.normalize_codepaths();
                i
            })
            .collect()
    };
    let open = read("workitems.jsonl");
    let closed = read("workitems_closed.jsonl");
    if open.is_empty() && closed.is_empty() {
        return Ok((0, 0));
    }

    let tx = conn.transaction()?;
    let mut seq = 0i64;
    let insert = |tx: &rusqlite::Transaction, item: &WorkItem, archived: bool, seq: i64| -> rusqlite::Result<()> {
        let body = serde_json::to_string(item).expect("workitem serializes");
        tx.execute(
            "INSERT OR REPLACE INTO workitems
               (workid, seq, archived, state, item_type, title, priority, source, created_by, added, closed, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                item.workid, seq, archived as i64, item.state, item.item_type, item.title,
                item.priority, item.source, item.created_by, item.times.added,
                item.times.closed, body,
            ],
        )?;
        Ok(())
    };
    for item in &open {
        seq += 1;
        insert(&tx, item, false, seq)?;
    }
    for item in &closed {
        seq += 1;
        insert(&tx, item, true, seq)?;
    }
    tx.commit()?;

    // Verify before retiring the files: a short count means something did not
    // round-trip, and the files must stay exactly where they are.
    let imported: i64 =
        conn.query_row("SELECT COUNT(*) FROM workitems", [], |r| r.get(0)).unwrap_or(0);
    let expected = (open.len() + closed.len()) as i64;
    if imported >= expected {
        for name in ["workitems.jsonl", "workitems_closed.jsonl"] {
            let from = dir.join(name);
            if from.exists() {
                let _ = std::fs::rename(&from, dir.join(format!("{}.imported", name)));
            }
        }
    }
    Ok((open.len(), closed.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("iter-db-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".iter/.engine")).unwrap();
        d
    }

    fn item(id: &str, state: &str) -> WorkItem {
        WorkItem { workid: id.into(), item_type: "code".into(), state: state.into(), ..Default::default() }
    }

    #[test]
    fn items_round_trip_and_keep_their_order() {
        let root = tmp("roundtrip");
        let conn = open(&root).unwrap();
        for (n, id) in ["a", "b", "c"].iter().enumerate() {
            let mut i = item(id, "queued");
            i.priority = n as i64;
            put_item(&conn, &i, false).unwrap();
        }
        let loaded = load_items(&conn, false);
        assert_eq!(loaded.iter().map(|i| i.workid.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);

        // An update must not reorder: the file form gave stable order for free
        // and the picker's tie-breaks still assume it.
        let mut b = loaded[1].clone();
        b.state = "paused".into();
        put_item(&conn, &b, false).unwrap();
        assert_eq!(
            load_items(&conn, false).iter().map(|i| i.workid.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The distinction the old two-file layout could not express: a `failed`
    /// item awaiting retry is OPEN, while a `failed` item that exhausted its
    /// attempts is archived. Deriving one from the state string would resurrect
    /// items mid-retry.
    #[test]
    fn archived_is_independent_of_state() {
        let root = tmp("archived");
        let conn = open(&root).unwrap();
        put_item(&conn, &item("retrying", "failed"), false).unwrap();
        put_item(&conn, &item("done-for", "failed"), true).unwrap();

        assert_eq!(load_items(&conn, false).len(), 1, "the retrying one is still open");
        assert_eq!(load_items(&conn, true).len(), 1);
        assert_eq!(counts(&conn)["failed"], 2, "counts span both sides");
        assert_eq!(counts(&conn)["open"], 1);
        assert_eq!(counts(&conn)["total"], 2);

        // Both are reachable by id — the lookup that used to miss archived rows.
        assert!(get_item(&conn, "done-for").is_some());
        assert!(!get_item(&conn, "done-for").unwrap().1, "reported as not-open");
        assert!(get_item(&conn, "retrying").unwrap().1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replace_open_adds_mutates_and_removes_without_touching_the_archive() {
        let root = tmp("replace");
        let mut conn = open(&root).unwrap();
        put_item(&conn, &item("keep", "queued"), false).unwrap();
        put_item(&conn, &item("drop", "queued"), false).unwrap();
        put_item(&conn, &item("archived", "complete"), true).unwrap();

        let mut keep = item("keep", "paused");
        keep.title = "mutated".into();
        replace_open(&mut conn, &[keep, item("fresh", "queued")]).unwrap();

        let open_now: Vec<String> = load_items(&conn, false).iter().map(|i| i.workid.clone()).collect();
        assert_eq!(open_now, ["keep", "fresh"], "removed, mutated and added in one step");
        assert_eq!(load_items(&conn, false)[0].state, "paused");
        assert_eq!(load_items(&conn, true).len(), 1, "the archive is not in scope");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn jsonl_import_runs_once_and_keeps_the_files_as_an_escape_hatch() {
        let root = tmp("import");
        let dir = crate::config::engine_dir(&root);
        std::fs::write(
            dir.join("workitems.jsonl"),
            format!("{}\n{}\n", json(&item("o1", "queued")), json(&item("o2", "todo"))),
        )
        .unwrap();
        std::fs::write(dir.join("workitems_closed.jsonl"), format!("{}\n", json(&item("c1", "complete")))).unwrap();

        let mut conn = open(&root).unwrap();
        assert_eq!(import_jsonl(&mut conn, &root).unwrap(), (2, 1));
        assert_eq!(load_items(&conn, false).len(), 2);
        assert_eq!(load_items(&conn, true).len(), 1);
        // Renamed, never deleted — the escape hatch stays until a clean week passes.
        assert!(!dir.join("workitems.jsonl").exists());
        assert!(dir.join("workitems.jsonl.imported").is_file());

        // Idempotent: a second call on a populated database imports nothing, so
        // this is safe to run on every startup.
        assert_eq!(import_jsonl(&mut conn, &root).unwrap(), (0, 0));
        assert_eq!(load_items(&conn, false).len(), 2, "no duplicates");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The wait this table exists to measure: one row per ask, closed by the
    /// answer. Re-asking the same unanswered question is the same wait, not a
    /// second one — `iter ask` and the engine's re-park both run through here.
    #[test]
    fn a_question_opens_one_wait_and_the_answer_closes_it() {
        let root = tmp("questions");
        let conn = open(&root).unwrap();
        record_question(&conn, "w1", "A or B?", "2026-08-26T10:00:00Z");
        record_question(&conn, "w1", "A or B?", "2026-08-26T10:05:00Z");
        let rows = export_table(&conn, "questions", ExportScope::All).unwrap();
        assert_eq!(rows.len(), 1, "re-asking the same open question is one wait: {:?}", rows);

        record_answer(&conn, "w1", "B, because of the lock scope", "2026-08-26T11:00:00Z");
        let row: serde_json::Value =
            serde_json::from_str(&export_table(&conn, "questions", ExportScope::All).unwrap()[0]).unwrap();
        assert_eq!(row["answer"], "B, because of the lock scope");
        assert_eq!(row["asked_at"], "2026-08-26T10:00:00Z", "the wait started at the FIRST ask");
        assert_eq!(row["answered_at"], "2026-08-26T11:00:00Z");

        // Asked again AFTER an answer: a genuinely new decision, so a new wait.
        record_question(&conn, "w1", "A or B?", "2026-08-26T12:00:00Z");
        assert_eq!(export_table(&conn, "questions", ExportScope::All).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two questions stacked up are answered oldest-first, and an answer with no
    /// open question is still kept — losing what a human actually said because
    /// the ask went unlogged would be the worse failure.
    #[test]
    fn answers_go_to_the_longest_waiting_question_and_are_never_dropped() {
        let root = tmp("answers");
        let conn = open(&root).unwrap();
        record_question(&conn, "w1", "first?", "2026-08-26T10:00:00Z");
        record_question(&conn, "w1", "second?", "2026-08-26T10:30:00Z");
        record_answer(&conn, "w1", "answering the first", "2026-08-26T11:00:00Z");
        let rows: Vec<serde_json::Value> = export_table(&conn, "questions", ExportScope::All)
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows[0]["question"], "first?");
        assert_eq!(rows[0]["answer"], "answering the first");
        assert_eq!(rows[1]["answer"], "", "the newer question is still waiting");

        record_answer(&conn, "never-asked", "said anyway", "2026-08-26T12:00:00Z");
        let rows = export_table(&conn, "questions", ExportScope::All).unwrap();
        assert_eq!(rows.len(), 3, "an orphan answer is recorded, not discarded");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The query Stephen asked for — "how often did the critic catch something
    /// real, per agent type" — has to be SQL over these rows, so rounds must
    /// stack per item and dispositions must land on the round they describe.
    #[test]
    fn critique_rounds_stack_and_take_dispositions() {
        let root = tmp("critiques");
        let conn = open(&root).unwrap();
        let record = |workid: &str, agent_type: &str| {
            let round = next_critique_round(&conn, workid);
            record_critique(&conn, workid, round, "_critic", agent_type, "plan.md", "VERDICT: …", "2026-08-26T10:00:00Z")
                .unwrap();
            round
        };
        assert_eq!(record("w1", "plan"), 1);
        assert_eq!(record("w1", "plan"), 2, "a second review of the same item stacks");
        assert_eq!(record("w2", "code"), 1, "rounds are per item, not global");

        // No round given targets the item's latest — what an agent coming
        // straight out of a review means when it does not say.
        assert!(set_critique_disposition(&conn, "w1", None, "revised").unwrap());
        assert!(set_critique_disposition(&conn, "w1", Some(1), "no-findings").unwrap());
        assert!(!set_critique_disposition(&conn, "w1", Some(9), "revised").unwrap(), "no such round");
        assert!(!set_critique_disposition(&conn, "nobody", None, "revised").unwrap());

        let rows: Vec<serde_json::Value> = export_table(&conn, "critiques", ExportScope::All)
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows[0]["disposition"], "no-findings", "round 1, named explicitly");
        assert_eq!(rows[1]["disposition"], "revised", "round 2, the latest");
        assert_eq!(rows[2]["agent_type"], "code");
        assert_eq!(rows[2]["disposition"], "", "unreported rounds stay empty");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The escape hatch: the queue stopped being a text file, so an exported
    /// line has to be a work item `iter add --file` would accept.
    #[test]
    fn exported_workitems_round_trip_and_the_archive_filters() {
        let root = tmp("export");
        let conn = open(&root).unwrap();
        let mut rich = item("open-1", "queued");
        rich.title = "has \"quotes\" and a\nnewline".into();
        rich.mainwork = "do the thing".into();
        rich.codepath = "src/".into();
        put_item(&conn, &rich, false).unwrap();
        put_item(&conn, &item("closed-1", "complete"), true).unwrap();

        let all = export_table(&conn, "workitems", ExportScope::All).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|l| !l.contains('\n')), "one item per line, always");
        assert_eq!(export_table(&conn, "workitems", ExportScope::Open).unwrap().len(), 1);
        let archived = export_table(&conn, "workitems", ExportScope::Archived).unwrap();
        assert_eq!(archived.len(), 1);
        assert!(archived[0].contains("closed-1"));

        // The round trip itself: parse a line back and it is the same item.
        let back: WorkItem = serde_json::from_str(&all[0]).unwrap();
        assert_eq!(back.workid, "open-1");
        assert_eq!(back.title, rich.title);
        assert_eq!(back.mainwork, "do the thing");
        assert_eq!(back.codepath, "src/");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An unknown table must be refused, not silently exported as nothing — and
    /// `table_count` is what keeps `iter status` from interpolating a caller's
    /// string into SQL.
    #[test]
    fn export_refuses_an_unknown_table_and_counts_only_known_ones() {
        let root = tmp("export-unknown");
        let conn = open(&root).unwrap();
        assert!(export_table(&conn, "sqlite_master", ExportScope::All).is_err());
        assert!(export_table(&conn, "workitems; DROP TABLE workitems", ExportScope::All).is_err());
        assert_eq!(table_count(&conn, "workitems; DROP TABLE workitems"), 0);
        put_item(&conn, &item("a", "queued"), false).unwrap();
        assert_eq!(table_count(&conn, "workitems"), 1);
        assert_eq!(table_count(&conn, "spend"), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn json(i: &WorkItem) -> String {
        serde_json::to_string(i).unwrap()
    }
}
