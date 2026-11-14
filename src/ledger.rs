//! Friction Ledger: append-only SQLite log of every hook event, every verdict
//! a jig gave, and every label a human put on a verdict.

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Ledger {
    conn: Connection,
}

pub struct EventRow<'a> {
    pub id: &'a str,
    pub ts_ms: i64,
    pub session: &'a str,
    pub event: &'a str,
    pub tool: Option<&'a str>,
    pub payload: &'a str,
}

pub struct VerdictRow<'a> {
    pub id: &'a str,
    pub ts_ms: i64,
    pub event_id: &'a str,
    pub session: &'a str,
    pub jig: &'a str,
    pub action: &'a str,
    pub lifecycle: &'a str,
    /// The agent was actually blocked or nudged (lifecycle was enforce).
    pub applied: bool,
    pub message: &'a str,
    pub evidence: &'a str,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub id: String,
    pub ts_ms: i64,
    pub event_id: String,
    pub session: String,
    pub jig: String,
    pub action: String,
    pub lifecycle: String,
    pub applied: bool,
    pub message: String,
    pub evidence: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JigCount {
    pub jig: String,
    pub fires: i64,
    pub applied: i64,
    pub wrong: i64,
    pub right: i64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, session TEXT NOT NULL,
  event TEXT NOT NULL, tool TEXT, payload TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS events_session_ts ON events(session, ts_ms);
CREATE TABLE IF NOT EXISTS verdicts (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, event_id TEXT NOT NULL REFERENCES events(id),
  session TEXT NOT NULL, jig TEXT NOT NULL, action TEXT NOT NULL, lifecycle TEXT NOT NULL,
  applied INTEGER NOT NULL, message TEXT NOT NULL, evidence TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS labels (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, verdict_id TEXT NOT NULL REFERENCES verdicts(id),
  label TEXT NOT NULL CHECK (label IN ('wrong','right')), note TEXT);
-- Content of a file the agent edited, taken when its turn stopped. Open
-- (checked = 0) until the next prompt of the session compares it.
CREATE TABLE IF NOT EXISTS snapshots (
  id TEXT PRIMARY KEY, session TEXT NOT NULL, path TEXT NOT NULL, ts_ms INTEGER NOT NULL,
  content TEXT NOT NULL, checked INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS snapshots_open ON snapshots(session, checked);
-- A role agent may run a deciding command (accept / reject) once: the hook
-- writes the grant when it sees that agent's call, from the agent_type
-- Claude Code puts in the hook input (not from anything the agent says),
-- and the command uses it up.
CREATE TABLE IF NOT EXISTS grants (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, op TEXT NOT NULL, target TEXT NOT NULL,
  agent_type TEXT NOT NULL, agent_id TEXT, used_ts_ms INTEGER);
-- Small facts about the ledger itself (e.g. when it was last pruned).
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- Proposals from Jig Smith. Kept when rejected, so the same idea is not
-- proposed again without saying what changed.
CREATE TABLE IF NOT EXISTS proposals (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, intent TEXT NOT NULL, outlet TEXT NOT NULL,
  draft TEXT NOT NULL, gate TEXT,
  status TEXT NOT NULL CHECK (status IN ('open','accepted','rejected','retired')),
  decided_by TEXT, why TEXT, decided_ts_ms INTEGER);
-- decided_by: 'gate', 'human', or 'agent:<agent_type>'.
-- Every run of a command jig: hookwatch's stopwatch on the jigs themselves.
CREATE TABLE IF NOT EXISTS jig_runs (
  id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, event_id TEXT NOT NULL REFERENCES events(id),
  jig TEXT NOT NULL, ms INTEGER NOT NULL, exit INTEGER,
  outcome TEXT NOT NULL CHECK (outcome IN ('pass','hit','error')), detail TEXT);
-- The human changed a file the agent had left, between two turns.
CREATE TABLE IF NOT EXISTS rework (
  id TEXT PRIMARY KEY, snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
  session TEXT NOT NULL, path TEXT NOT NULL, ts_ms INTEGER NOT NULL,
  added INTEGER NOT NULL, removed INTEGER NOT NULL);
";

#[derive(Debug, Clone)]
pub struct Event {
    pub ts_ms: i64,
    pub session: String,
    pub event: String,
    pub tool: Option<String>,
    pub payload: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PruneReport {
    /// Rows where a credential was replaced (events, snapshots, rework).
    pub redacted: usize,
    /// PostToolUse events older than the cutoff whose tool output was dropped.
    pub outputs_dropped: usize,
    /// Checked snapshots older than the cutoff whose content was dropped.
    pub snapshots_emptied: usize,
    /// Event rows that were not JSON, replaced by a row that says the payload
    /// was lost.
    pub repaired: usize,
    pub bytes_before: i64,
    pub bytes_after: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub events: i64,
    pub payload_bytes: i64,
    pub open_proposals: i64,
    /// Verdicts that acted on the agent and have no human label yet.
    pub unlabelled_applied: i64,
    pub last_prune_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ProposalRow {
    pub id: String,
    pub ts_ms: i64,
    pub intent: String,
    pub outlet: String,
    pub draft: String,
    pub gate: Option<String>,
    pub status: String,
    pub decided_by: Option<String>,
    pub why: Option<String>,
}

const PROPOSAL_COLS: &str = "id, ts_ms, intent, outlet, draft, gate, status, decided_by, why";

fn proposal_from(r: &rusqlite::Row) -> rusqlite::Result<ProposalRow> {
    Ok(ProposalRow {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        intent: r.get(2)?,
        outlet: r.get(3)?,
        draft: r.get(4)?,
        gate: r.get(5)?,
        status: r.get(6)?,
        decided_by: r.get(7)?,
        why: r.get(8)?,
    })
}

#[derive(Debug, Clone)]
pub struct RunStats {
    pub jig: String,
    pub runs: i64,
    pub hits: i64,
    pub errors: i64,
    /// Durations, sorted.
    pub ms: Vec<i64>,
}

/// What `close_snapshot` records when the human changed the file.
#[derive(Debug, Clone)]
pub struct ReworkFound {
    pub id: String,
    pub ts_ms: i64,
    pub added: usize,
    pub removed: usize,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct ReworkRow {
    pub session: String,
    pub path: String,
    pub ts_ms: i64,
    pub added: i64,
    pub removed: i64,
    pub diff: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LabelRow {
    pub ts_ms: i64,
    /// When the verdict was given (the tool call it judged).
    pub verdict_ts_ms: i64,
    pub session: String,
    pub verdict_id: String,
    pub jig: String,
    pub evidence: String,
    pub label: String,
    pub note: Option<String>,
}

/// Bring a ledger made by an older hookwatch up to the current schema.
fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let has_diff: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('rework') WHERE name = 'diff'",
        [],
        |r| r.get(0),
    )?;
    if !has_diff {
        conn.execute("ALTER TABLE rework ADD COLUMN diff TEXT", [])?;
    }
    // decided_by was limited to gate / human; a role agent decides too now.
    let old_check: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE name = 'proposals' AND sql LIKE '%decided_by IN%'",
        [],
        |r| r.get(0),
    )?;
    if old_check {
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE proposals RENAME TO proposals_old;
             CREATE TABLE proposals (
               id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, intent TEXT NOT NULL, outlet TEXT NOT NULL,
               draft TEXT NOT NULL, gate TEXT,
               status TEXT NOT NULL CHECK (status IN ('open','accepted','rejected','retired')),
               decided_by TEXT, why TEXT, decided_ts_ms INTEGER);
             INSERT INTO proposals SELECT * FROM proposals_old;
             DROP TABLE proposals_old;
             COMMIT;",
        )?;
    }
    Ok(())
}

const VERDICT_COLS: &str = "v.id, v.ts_ms, v.event_id, v.session, v.jig, v.action, v.lifecycle, v.applied, v.message, v.evidence,
  (SELECT l.label FROM labels l WHERE l.verdict_id = v.id ORDER BY l.ts_ms DESC LIMIT 1)";

fn verdict_from(r: &rusqlite::Row) -> rusqlite::Result<Verdict> {
    Ok(Verdict {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        event_id: r.get(2)?,
        session: r.get(3)?,
        jig: r.get(4)?,
        action: r.get(5)?,
        lifecycle: r.get(6)?,
        applied: r.get::<_, i64>(7)? != 0,
        message: r.get(8)?,
        evidence: r.get(9)?,
        label: r.get(10)?,
    })
}

impl Ledger {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        // Hooks of parallel sessions write at the same time.
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Ledger { conn })
    }

    /// Payloads of the PostToolUse events of `session` since its last prompt
    /// (all of them when no prompt was recorded: hookwatch came in mid-session).
    pub fn post_tool_payloads_this_turn(&self, session: &str) -> anyhow::Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT payload FROM events
             WHERE session = ?1 AND event = 'PostToolUse'
               AND ts_ms >= COALESCE((SELECT MAX(ts_ms) FROM events
                                      WHERE session = ?1 AND event = 'UserPromptSubmit'), 0)
             ORDER BY ts_ms",
        )?;
        let rows = st.query_map([session], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Take a snapshot. An open snapshot of the same file in the session is
    /// replaced: a later stop of the same turn knows the newer content.
    pub fn put_snapshot(
        &self,
        id: &str,
        session: &str,
        path: &str,
        ts_ms: i64,
        content: &str,
    ) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM snapshots WHERE session = ?1 AND path = ?2 AND checked = 0",
            params![session, path],
        )?;
        tx.execute(
            "INSERT INTO snapshots (id, session, path, ts_ms, content) VALUES (?1,?2,?3,?4,?5)",
            params![id, session, path, ts_ms, content],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn open_snapshots(&self, session: &str) -> anyhow::Result<Vec<Snapshot>> {
        let mut st = self.conn.prepare(
            "SELECT id, path, content FROM snapshots WHERE session = ?1 AND checked = 0 ORDER BY path",
        )?;
        let rows = st.query_map([session], |r| {
            Ok(Snapshot {
                id: r.get(0)?,
                path: r.get(1)?,
                content: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Close a snapshot, with the rework found (if any) in the same transaction.
    pub fn close_snapshot(
        &self,
        snap: &Snapshot,
        session: &str,
        rework: Option<ReworkFound>,
    ) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(r) = rework {
            tx.execute(
                "INSERT INTO rework (id, snapshot_id, session, path, ts_ms, added, removed, diff)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    r.id,
                    snap.id,
                    session,
                    snap.path,
                    r.ts_ms,
                    r.added as i64,
                    r.removed as i64,
                    r.diff
                ],
            )?;
        }
        tx.execute("UPDATE snapshots SET checked = 1 WHERE id = ?1", [&snap.id])?;
        tx.commit()?;
        Ok(())
    }

    /// Store a proposal. A gate that failed closes it at once as rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn add_proposal(
        &self,
        id: &str,
        ts_ms: i64,
        intent: &str,
        outlet: &str,
        draft: &str,
        gate: Option<&str>,
        rejected_by_gate: Option<&str>,
    ) -> anyhow::Result<()> {
        let (status, by, why) = match rejected_by_gate {
            Some(w) => ("rejected", Some("gate"), Some(w)),
            None => ("open", None, None),
        };
        let decided = by.map(|_| ts_ms);
        self.conn.execute(
            "INSERT INTO proposals (id, ts_ms, intent, outlet, draft, gate, status, decided_by, why, decided_ts_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id, ts_ms, intent, outlet, draft, gate, status, by, why, decided],
        )?;
        Ok(())
    }

    /// All proposals, newest first.
    pub fn proposals(&self) -> anyhow::Result<Vec<ProposalRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLS} FROM proposals ORDER BY ts_ms DESC"
        ))?;
        let rows = st.query_map([], proposal_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One proposal by the start or end of its id; more than one match is an error.
    pub fn proposal_by_short(&self, part: &str) -> anyhow::Result<ProposalRow> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLS} FROM proposals WHERE id LIKE ?1 || '%' OR id LIKE '%' || ?1 LIMIT 2"
        ))?;
        let mut rows: Vec<ProposalRow> = st
            .query_map([part], proposal_from)?
            .collect::<Result<_, _>>()?;
        match rows.len() {
            0 => anyhow::bail!("no proposal with id {part}"),
            1 => Ok(rows.remove(0)),
            _ => anyhow::bail!("id {part} matches more than one proposal; give more characters"),
        }
    }

    /// (id, payload) of the session's events since its last prompt.
    pub fn turn_rows(&self, session: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare(
            "SELECT id, payload FROM events
             WHERE session = ?1
               AND ts_ms >= COALESCE((SELECT MAX(ts_ms) FROM events
                                      WHERE session = ?1 AND event = 'UserPromptSubmit'), 0)",
        )?;
        let rows = st.query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_payload(&self, id: &str, payload: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE events SET payload = ?2 WHERE id = ?1",
            params![id, payload],
        )?;
        Ok(())
    }

    /// Every stored text that may hold a credential: event payloads,
    /// snapshot contents, rework diffs. For one scan before `prune`.
    pub fn all_texts(&self) -> anyhow::Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT payload FROM events UNION ALL SELECT content FROM snapshots
             UNION ALL SELECT diff FROM rework WHERE diff IS NOT NULL",
        )?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn stored_bytes(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE((SELECT SUM(length(payload)) FROM events), 0)
                  + COALESCE((SELECT SUM(length(content)) FROM snapshots), 0)
                  + COALESCE((SELECT SUM(length(diff)) FROM rework), 0)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Redact every stored text with `redact`, drop tool outputs of
    /// PostToolUse events before `cutoff_ms` (replay and episodes use only
    /// tool_input), and empty checked snapshots before it. One transaction;
    /// the caller vacuums after. Rows that are not JSON stop it.
    pub fn prune(
        &self,
        cutoff_ms: i64,
        now_ms: i64,
        redact: &dyn Fn(&str) -> (String, usize),
    ) -> anyhow::Result<PruneReport> {
        let mut rep = PruneReport {
            bytes_before: self.stored_bytes()?,
            ..Default::default()
        };
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("SELECT id, ts_ms, event, payload FROM events")?;
            let rows: Vec<(String, i64, String, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            for (id, ts, event, payload) in rows {
                let (mut text, n) = redact(&payload);
                if n > 0 {
                    rep.redacted += 1;
                }
                let parsed = serde_json::from_str::<serde_json::Value>(&text);
                let Ok(mut v) = parsed else {
                    // The tail of these rows is gone (the regex masking of
                    // 2026-11-14 cut it). Keep a readable row that says so,
                    // with what the columns still know.
                    let (session, tool): (String, Option<String>) = tx.query_row(
                        "SELECT session, tool FROM events WHERE id = ?1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    text = serde_json::json!({
                        "hook_event_name": event, "session_id": session, "tool_name": tool,
                        "hookwatch_damaged": "payload lost: it was not JSON when pruned",
                    })
                    .to_string();
                    rep.repaired += 1;
                    tx.execute(
                        "UPDATE events SET payload = ?2 WHERE id = ?1",
                        params![id, text],
                    )?;
                    continue;
                };
                if event == "PostToolUse"
                    && ts < cutoff_ms
                    && let Some(obj) = v.as_object_mut()
                    && obj.remove("tool_response").is_some()
                {
                    rep.outputs_dropped += 1;
                    text = v.to_string();
                }
                if text != payload {
                    tx.execute(
                        "UPDATE events SET payload = ?2 WHERE id = ?1",
                        params![id, text],
                    )?;
                }
            }
        }
        {
            let mut st = tx.prepare("SELECT id, ts_ms, checked, content FROM snapshots")?;
            let rows: Vec<(String, i64, i64, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            for (id, ts, checked, content) in rows {
                let new = if checked != 0 && ts < cutoff_ms && !content.is_empty() {
                    rep.snapshots_emptied += 1;
                    String::new()
                } else {
                    let (t, n) = redact(&content);
                    if n > 0 {
                        rep.redacted += 1;
                    }
                    t
                };
                if new != content {
                    tx.execute(
                        "UPDATE snapshots SET content = ?2 WHERE id = ?1",
                        params![id, new],
                    )?;
                }
            }
        }
        {
            let mut st = tx.prepare("SELECT id, diff FROM rework WHERE diff IS NOT NULL")?;
            let rows: Vec<(String, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (id, diff) in rows {
                let (t, n) = redact(&diff);
                if n > 0 {
                    rep.redacted += 1;
                    tx.execute("UPDATE rework SET diff = ?2 WHERE id = ?1", params![id, t])?;
                }
            }
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('last_prune_ms', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [now_ms.to_string()],
        )?;
        tx.commit()?;
        rep.bytes_after = self.stored_bytes()?;
        Ok(rep)
    }

    /// A consistent copy of the whole ledger (SQLite VACUUM INTO).
    pub fn backup_to(&self, path: &Path) -> anyhow::Result<()> {
        let p = path.to_str().context("backup path is not UTF-8")?;
        self.conn
            .execute("VACUUM INTO ?1", [p])
            .with_context(|| format!("back up to {p}"))?;
        Ok(())
    }

    /// Give the freed pages back to the file system.
    pub fn vacuum(&self) -> anyhow::Result<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    pub fn status(&self) -> anyhow::Result<Status> {
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_prune_ms'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let last_prune_ms = match last {
            Some(s) => Some(
                s.parse()
                    .with_context(|| format!("meta last_prune_ms is not a number: {s}"))?,
            ),
            None => None, // never pruned
        };
        Ok(self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM events),
                    (SELECT COALESCE(SUM(length(payload)), 0) FROM events),
                    (SELECT COUNT(*) FROM proposals WHERE status = 'open'),
                    (SELECT COUNT(*) FROM verdicts v WHERE v.applied = 1
                       AND NOT EXISTS (SELECT 1 FROM labels l WHERE l.verdict_id = v.id))",
            [],
            |r| {
                Ok(Status {
                    events: r.get(0)?,
                    payload_bytes: r.get(1)?,
                    open_proposals: r.get(2)?,
                    unlabelled_applied: r.get(3)?,
                    last_prune_ms,
                })
            },
        )?)
    }

    /// Whether this jig already gave this evidence (for `repeat: once`).
    pub fn has_verdict(&self, jig: &str, evidence: &str) -> anyhow::Result<bool> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM verdicts WHERE jig = ?1 AND evidence = ?2",
            params![jig, evidence],
            |r| r.get(0),
        )?)
    }

    pub fn add_grant(
        &self,
        id: &str,
        ts_ms: i64,
        op: &str,
        target: &str,
        agent_type: &str,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO grants (id, ts_ms, op, target, agent_type, agent_id) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, ts_ms, op, target, agent_type, agent_id],
        )?;
        Ok(())
    }

    /// Use up the newest unused grant for (op, target) younger than
    /// `max_age_ms`; its agent_type, or None.
    pub fn take_grant(
        &self,
        op: &str,
        target: &str,
        now_ms: i64,
        max_age_ms: i64,
    ) -> anyhow::Result<Option<String>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, agent_type FROM grants WHERE op = ?1 AND target = ?2
                 AND used_ts_ms IS NULL AND ts_ms >= ?3 ORDER BY ts_ms DESC LIMIT 1",
                params![op, target, now_ms - max_age_ms],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, agent_type)) = row else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE grants SET used_ts_ms = ?2 WHERE id = ?1",
            params![id, now_ms],
        )?;
        Ok(Some(agent_type))
    }

    /// (tool_name, tool_input) of every call whose verdict the human last
    /// labelled right: hits a human has confirmed.
    pub fn right_labelled_calls(&self) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let mut st = self.conn.prepare(
            "SELECT e.payload FROM verdicts v JOIN events e ON e.id = v.event_id
             WHERE (SELECT l.label FROM labels l WHERE l.verdict_id = v.id ORDER BY l.ts_ms DESC LIMIT 1) = 'right'",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for p in rows {
            let v: serde_json::Value =
                serde_json::from_str(&p?).context("ledger payload is not JSON")?;
            let tool = v
                .get("tool_name")
                .and_then(|t| t.as_str())
                .context("labelled call has no tool_name")?;
            out.push((tool.to_owned(), v["tool_input"].clone()));
        }
        Ok(out)
    }

    /// A human decides an open proposal. Deciding a closed one is an error.
    pub fn decide_proposal(
        &self,
        id: &str,
        status: &str,
        why: Option<&str>,
        ts_ms: i64,
        by: &str,
    ) -> anyhow::Result<()> {
        let n = self.conn.execute(
            "UPDATE proposals SET status = ?2, decided_by = ?5, why = ?3, decided_ts_ms = ?4
             WHERE id = ?1 AND status = 'open'",
            params![id, status, why, ts_ms, by],
        )?;
        if n != 1 {
            anyhow::bail!("proposal {id} is not open");
        }
        Ok(())
    }

    pub fn add_run(
        &self,
        id: &str,
        ts_ms: i64,
        event_id: &str,
        run: &crate::jig::Run,
    ) -> anyhow::Result<()> {
        let detail = match &run.outcome {
            crate::jig::RunOutcome::Error(m) => Some(m.as_str()),
            _ => None,
        };
        self.conn.execute(
            "INSERT INTO jig_runs (id, ts_ms, event_id, jig, ms, exit, outcome, detail) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, ts_ms, event_id, run.jig_id, run.ms as i64, run.exit, run.outcome.as_str(), detail],
        )?;
        Ok(())
    }

    /// Per command jig: (jig, runs, hits, errors, durations in ms sorted).
    pub fn run_stats(&self) -> anyhow::Result<Vec<RunStats>> {
        let mut st = self
            .conn
            .prepare("SELECT jig, ms, outcome FROM jig_runs ORDER BY jig, ms")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out: Vec<RunStats> = vec![];
        for row in rows {
            let (jig, ms, outcome) = row?;
            if out.last().is_none_or(|s| s.jig != jig) {
                out.push(RunStats {
                    jig: jig.clone(),
                    runs: 0,
                    hits: 0,
                    errors: 0,
                    ms: vec![],
                });
            }
            let s = out.last_mut().expect("pushed above");
            s.runs += 1;
            s.ms.push(ms);
            match outcome.as_str() {
                "hit" => s.hits += 1,
                "error" => s.errors += 1,
                _ => {}
            }
        }
        Ok(out)
    }

    /// Every rework, oldest first. `diff` is None for rows recorded before
    /// hookwatch kept the diff text.
    pub fn rework_rows(&self) -> anyhow::Result<Vec<ReworkRow>> {
        let mut st = self.conn.prepare(
            "SELECT session, path, ts_ms, added, removed, diff FROM rework ORDER BY ts_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ReworkRow {
                session: r.get(0)?,
                path: r.get(1)?,
                ts_ms: r.get(2)?,
                added: r.get(3)?,
                removed: r.get(4)?,
                diff: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every label a human put on a verdict, oldest first.
    pub fn label_rows(&self) -> anyhow::Result<Vec<LabelRow>> {
        let mut st = self.conn.prepare(
            "SELECT l.ts_ms, v.ts_ms, v.session, v.id, v.jig, v.evidence, l.label, l.note
             FROM labels l JOIN verdicts v ON v.id = l.verdict_id ORDER BY l.ts_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok(LabelRow {
                ts_ms: r.get(0)?,
                verdict_ts_ms: r.get(1)?,
                session: r.get(2)?,
                verdict_id: r.get(3)?,
                jig: r.get(4)?,
                evidence: r.get(5)?,
                label: r.get(6)?,
                note: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    #[cfg(test)]
    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Ledger { conn })
    }

    pub fn add_event(&self, e: &EventRow) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO events (id, ts_ms, session, event, tool, payload) VALUES (?1,?2,?3,?4,?5,?6)",
            params![e.id, e.ts_ms, e.session, e.event, e.tool, e.payload],
        )?;
        Ok(())
    }

    pub fn add_verdict(&self, v: &VerdictRow) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO verdicts (id, ts_ms, event_id, session, jig, action, lifecycle, applied, message, evidence)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                v.id, v.ts_ms, v.event_id, v.session, v.jig, v.action, v.lifecycle,
                v.applied as i64, v.message, v.evidence
            ],
        )?;
        Ok(())
    }

    pub fn add_label(
        &self,
        id: &str,
        ts_ms: i64,
        verdict_id: &str,
        label: &str,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO labels (id, ts_ms, verdict_id, label, note) VALUES (?1,?2,?3,?4,?5)",
            params![id, ts_ms, verdict_id, label, note],
        )?;
        Ok(())
    }

    pub fn recent_verdicts(&self, limit: i64) -> anyhow::Result<Vec<Verdict>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {VERDICT_COLS} FROM verdicts v ORDER BY v.ts_ms DESC LIMIT ?1"
        ))?;
        let rows = st.query_map([limit], verdict_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Find one verdict by the start or the end of its id (the CLI shows the
    /// last 8 characters: the random part of a UUID v7). More than one match
    /// is an error.
    pub fn verdict_by_short(&self, part: &str) -> anyhow::Result<Verdict> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {VERDICT_COLS} FROM verdicts v
             WHERE v.id LIKE ?1 || '%' OR v.id LIKE '%' || ?1 LIMIT 2"
        ))?;
        let mut rows: Vec<Verdict> = st
            .query_map([part], verdict_from)?
            .collect::<Result<_, _>>()?;
        match rows.len() {
            0 => anyhow::bail!("no verdict with id {part}"),
            1 => Ok(rows.remove(0)),
            _ => anyhow::bail!("id {part} matches more than one verdict; give more characters"),
        }
    }

    /// The newest `last` events (filtered by session prefix and event names),
    /// returned oldest first.
    pub fn recent_events(
        &self,
        session: Option<&str>,
        kinds: &[String],
        last: i64,
    ) -> anyhow::Result<Vec<Event>> {
        let kinds = kinds.join(",");
        let mut st = self.conn.prepare(
            "SELECT ts_ms, session, event, tool, payload FROM events
             WHERE (?1 IS NULL OR session LIKE ?1 || '%')
               AND (?2 = '' OR instr(',' || ?2 || ',', ',' || event || ',') > 0)
             ORDER BY ts_ms DESC, id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![session, kinds, last], |r| {
            Ok(Event {
                ts_ms: r.get(0)?,
                session: r.get(1)?,
                event: r.get(2)?,
                tool: r.get(3)?,
                payload: r.get(4)?,
            })
        })?;
        let mut out: Vec<Event> = rows.collect::<Result<_, _>>()?;
        out.reverse();
        Ok(out)
    }

    pub fn event_payload(&self, event_id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT payload FROM events WHERE id = ?1",
                [event_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every event, oldest first per session.
    pub fn event_stream(&self) -> anyhow::Result<Vec<Event>> {
        let mut st = self.conn.prepare(
            "SELECT ts_ms, session, event, tool, payload FROM events ORDER BY session, ts_ms, id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Event {
                ts_ms: r.get(0)?,
                session: r.get(1)?,
                event: r.get(2)?,
                tool: r.get(3)?,
                payload: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn jig_counts(&self) -> anyhow::Result<Vec<JigCount>> {
        let mut st = self.conn.prepare(
            "SELECT jig, COUNT(*), SUM(applied),
                    COALESCE(SUM(label = 'wrong'), 0), COALESCE(SUM(label = 'right'), 0)
             FROM (SELECT v.jig AS jig, v.applied AS applied,
                     (SELECT l.label FROM labels l WHERE l.verdict_id = v.id ORDER BY l.ts_ms DESC LIMIT 1) AS label
                   FROM verdicts v)
             GROUP BY jig ORDER BY COUNT(*) DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(JigCount {
                jig: r.get(0)?,
                fires: r.get(1)?,
                applied: r.get(2)?,
                wrong: r.get(3)?,
                right: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(l: &Ledger, id: &str, jig: &str, applied: bool) {
        l.add_verdict(&VerdictRow {
            id,
            ts_ms: 1,
            event_id: "e1",
            session: "s",
            jig,
            action: "block",
            lifecycle: if applied { "enforce" } else { "shadow" },
            applied,
            message: "m",
            evidence: "x",
        })
        .unwrap();
    }

    fn ev(l: &Ledger, id: &str, ts: i64, event: &str, payload: &str) {
        l.add_event(&EventRow {
            id,
            ts_ms: ts,
            session: "s",
            event,
            tool: None,
            payload,
        })
        .unwrap();
    }

    #[test]
    fn turn_payloads_start_at_the_last_prompt() {
        let l = Ledger::in_memory().unwrap();
        ev(&l, "1", 1, "PostToolUse", "old-turn");
        ev(&l, "2", 2, "UserPromptSubmit", "{}");
        ev(&l, "3", 3, "PostToolUse", "this-turn");
        ev(&l, "4", 4, "PreToolUse", "pre");
        assert_eq!(
            l.post_tool_payloads_this_turn("s").unwrap(),
            vec!["this-turn"]
        );
        assert!(l.post_tool_payloads_this_turn("other").unwrap().is_empty());
    }

    #[test]
    fn snapshot_is_replaced_while_open_and_rework_counted_per_turn() {
        let l = Ledger::in_memory().unwrap();
        l.put_snapshot("a1", "s", "/r/a", 10, "v1").unwrap();
        l.put_snapshot("a2", "s", "/r/a", 11, "v2").unwrap();
        l.put_snapshot("b1", "s", "/r/b", 11, "x").unwrap();
        let open = l.open_snapshots("s").unwrap();
        assert_eq!(
            open.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["a2", "b1"]
        );
        let found = |id: &str, added, removed| ReworkFound {
            id: id.into(),
            ts_ms: 20,
            added,
            removed,
            diff: format!("diff of {id}"),
        };
        l.close_snapshot(&open[0], "s", Some(found("r1", 2, 1)))
            .unwrap();
        l.close_snapshot(&open[1], "s", Some(found("r2", 1, 0)))
            .unwrap();
        assert!(l.open_snapshots("s").unwrap().is_empty());
        let rows = l.rework_rows().unwrap();
        assert_eq!(rows[0].diff.as_deref(), Some("diff of r1"));
        // Both files closed by one check: one turn, two files, +3 -1.
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.ts_ms == 20 && r.session == "s"));
        let added: i64 = rows.iter().map(|r| r.added).sum();
        let removed: i64 = rows.iter().map(|r| r.removed).sum();
        assert_eq!((added, removed), (3, 1));
    }

    #[test]
    fn prune_drops_old_outputs_and_snapshots_and_redacts_everything() {
        let l = Ledger::in_memory().unwrap();
        let post = |id, ts, out: &str| {
            let p = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": out});
            l.add_event(&EventRow {
                id,
                ts_ms: ts,
                session: "s",
                event: "PostToolUse",
                tool: Some("Bash"),
                payload: &p.to_string(),
            })
            .unwrap();
        };
        post("old", 10, "SECRETWORD in an old output");
        post("new", 100, "fresh output");
        l.put_snapshot("snap-old", "s", "/r/a", 10, "old text")
            .unwrap();
        let open = l.open_snapshots("s").unwrap();
        l.close_snapshot(&open[0], "s", None).unwrap();
        l.put_snapshot("snap-new", "s", "/r/b", 100, "has SECRETWORD")
            .unwrap();
        // A stand-in for jig::redact, so this test holds no credential.
        let redact = |t: &str| {
            (
                t.replace("SECRETWORD", "[R]"),
                t.matches("SECRETWORD").count(),
            )
        };

        let r = l.prune(50, 200, &redact).unwrap();
        assert_eq!(
            (r.redacted, r.outputs_dropped, r.snapshots_emptied),
            (2, 1, 1)
        );
        assert!(r.bytes_after < r.bytes_before);
        let payload = |id: &str| -> serde_json::Value {
            serde_json::from_str(
                &l.conn
                    .query_row("SELECT payload FROM events WHERE id = ?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                    .unwrap(),
            )
            .unwrap()
        };
        assert!(payload("old").get("tool_response").is_none());
        assert_eq!(payload("old")["tool_input"]["command"], "ls"); // replay still has the call
        assert_eq!(payload("new")["tool_response"], "fresh output");
        assert_eq!(l.open_snapshots("s").unwrap()[0].content, "has [R]");
        assert_eq!(l.status().unwrap().last_prune_ms, Some(200));
        // Twice is a no-op.
        let r = l.prune(50, 300, &redact).unwrap();
        assert_eq!(
            (r.redacted, r.outputs_dropped, r.snapshots_emptied),
            (0, 0, 0)
        );
    }

    #[test]
    fn a_grant_is_used_once_and_only_while_fresh() {
        let l = Ledger::in_memory().unwrap();
        l.add_grant("g1", 1_000, "accept", "abc", "hookwatch:jig-judge", None)
            .unwrap();
        assert_eq!(
            l.take_grant("accept", "abc", 200_000, 60_000).unwrap(),
            None
        ); // stale
        l.add_grant("g2", 100_000, "accept", "abc", "hookwatch:jig-judge", None)
            .unwrap();
        assert_eq!(
            l.take_grant("reject", "abc", 100_500, 60_000).unwrap(),
            None
        );
        assert_eq!(
            l.take_grant("accept", "abc", 100_500, 60_000)
                .unwrap()
                .as_deref(),
            Some("hookwatch:jig-judge")
        );
        assert_eq!(
            l.take_grant("accept", "abc", 100_600, 60_000).unwrap(),
            None
        ); // used
    }

    #[test]
    fn old_proposals_table_is_rebuilt_to_take_an_agent_decider() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE proposals (
               id TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, intent TEXT NOT NULL, outlet TEXT NOT NULL,
               draft TEXT NOT NULL, gate TEXT,
               status TEXT NOT NULL CHECK (status IN ('open','accepted','rejected','retired')),
               decided_by TEXT CHECK (decided_by IN ('gate','human')), why TEXT, decided_ts_ms INTEGER);
             INSERT INTO proposals VALUES ('p1', 1, 'i', 'jig', '{}', NULL, 'open', NULL, NULL, NULL);",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        let l = Ledger { conn };
        l.decide_proposal("p1", "accepted", None, 2, "agent:hookwatch:jig-judge")
            .unwrap();
        assert_eq!(
            l.proposal_by_short("p1").unwrap().decided_by.as_deref(),
            Some("agent:hookwatch:jig-judge")
        );
    }

    /// The rows the regex masking of 2026-11-14 cut: prune makes them
    /// readable again and says the payload was lost.
    #[test]
    fn prune_repairs_rows_that_are_not_json() {
        let l = Ledger::in_memory().unwrap();
        l.add_event(&EventRow {
            id: "cut",
            ts_ms: 1,
            session: "s",
            event: "PreToolUse",
            tool: Some("Edit"),
            payload: r#"{"tool_input":{"x":"abc"#,
        })
        .unwrap();
        let r = l.prune(0, 2, &|t: &str| (t.to_owned(), 0)).unwrap();
        assert_eq!(r.repaired, 1);
        let p: serde_json::Value = serde_json::from_str(
            &l.conn
                .query_row("SELECT payload FROM events WHERE id = 'cut'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (p["tool_name"].as_str(), p["session_id"].as_str()),
            (Some("Edit"), Some("s"))
        );
        assert!(p.get("hookwatch_damaged").is_some());
    }

    #[test]
    fn proposal_gate_rejection_is_kept_and_open_ones_are_decided_once() {
        let l = Ledger::in_memory().unwrap();
        l.add_proposal("p1", 1, "i", "jig", "{}", Some("{}"), None)
            .unwrap();
        l.add_proposal(
            "p2",
            2,
            "i",
            "jig",
            "{}",
            Some("{}"),
            Some("2 unconfirmed hits"),
        )
        .unwrap();
        let all = l.proposals().unwrap();
        assert_eq!(
            all.iter().map(|p| p.status.as_str()).collect::<Vec<_>>(),
            vec!["rejected", "open"]
        );
        assert_eq!(all[0].decided_by.as_deref(), Some("gate"));
        assert!(
            l.decide_proposal("p2", "accepted", None, 3, "human")
                .is_err()
        );
        l.decide_proposal("p1", "accepted", None, 3, "human")
            .unwrap();
        assert!(
            l.decide_proposal("p1", "rejected", Some("again"), 4, "human")
                .is_err()
        );
        assert_eq!(
            l.proposal_by_short("p1").unwrap().decided_by.as_deref(),
            Some("human")
        );
    }

    /// A ledger made by V0 (rework without `diff`) opens and gains the column.
    #[test]
    fn old_ledger_is_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE rework (id TEXT PRIMARY KEY, snapshot_id TEXT NOT NULL, session TEXT NOT NULL,
               path TEXT NOT NULL, ts_ms INTEGER NOT NULL, added INTEGER NOT NULL, removed INTEGER NOT NULL);
             INSERT INTO rework VALUES ('r0','s0','s','/r/a',1,1,1);",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // twice is a no-op
        let l = Ledger { conn };
        assert_eq!(l.rework_rows().unwrap()[0].diff, None);
    }

    #[test]
    fn counts_use_the_latest_label_and_prefix_must_be_unique() {
        let l = Ledger::in_memory().unwrap();
        l.add_event(&EventRow {
            id: "e1",
            ts_ms: 1,
            session: "s",
            event: "PreToolUse",
            tool: Some("Bash"),
            payload: "{}",
        })
        .unwrap();
        verdict(&l, "aaa1", "no-verify", true);
        verdict(&l, "aaa2", "no-verify", false);
        verdict(&l, "bbb1", "tamper-guard", true);
        l.add_label("l1", 10, "aaa1", "right", None).unwrap();
        l.add_label("l2", 20, "aaa1", "wrong", Some("changed my mind"))
            .unwrap();
        l.add_label("l3", 10, "bbb1", "right", None).unwrap();

        let c = l.jig_counts().unwrap();
        let nv = c.iter().find(|c| c.jig == "no-verify").unwrap();
        assert_eq!((nv.fires, nv.applied, nv.wrong, nv.right), (2, 1, 1, 0));
        let tg = c.iter().find(|c| c.jig == "tamper-guard").unwrap();
        assert_eq!((tg.fires, tg.wrong, tg.right), (1, 0, 1));

        assert!(l.verdict_by_short("aaa").is_err());
        assert_eq!(
            l.verdict_by_short("aaa1").unwrap().label.as_deref(),
            Some("wrong")
        );
        assert!(l.verdict_by_short("zzz").is_err());
        assert_eq!(l.verdict_by_short("bb1").unwrap().id, "bbb1");
    }
}
