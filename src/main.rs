//! Friction-measured hook engine (jigs) for coding agents.
//!
//! `hookwatch hook` is the command Claude Code runs on every hook event. It
//! writes the event to the Friction Ledger and, on PreToolUse, runs the L1
//! jigs. A jig in `enforce` blocks (exit 2) or nudges; a jig in `shadow` only
//! records what it would have done.

mod episode;
mod friction;
mod jig;
mod ledger;
mod mask;
mod proposal;
mod replay;
mod rework;
mod smith;

use anyhow::Context;
use clap::{Parser, Subcommand};
use jig::{Action, Engine, Jig, JigFile, Lifecycle, ToolCall};
use ledger::{EventRow, Ledger, VerdictRow};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run as a Claude Code hook: event JSON on stdin.
    Hook,
    /// Show recent verdicts, or one verdict in full.
    Why { id: Option<String> },
    /// Label a verdict: the human says the jig was wrong or right. Needs a
    /// terminal, so an agent cannot label its own verdicts.
    Override {
        id: String,
        #[arg(long, conflicts_with = "right", required_unless_present = "right")]
        wrong: bool,
        #[arg(long)]
        right: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Friction measures and per-jig counts.
    Stats {
        /// Only sessions that worked in this repo (by the hook's cwd).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// The ledger's size and what waits for the human (open proposals,
    /// unlabelled verdicts). Also shown at session start when it matters.
    Status,
    /// Everything a management view shows, as one JSON: status, the jigs
    /// with their counts, recent proposals and verdicts, and the stopwatch
    /// (of --repo when given). Read-only.
    Panel {
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Credentials in stored rows are replaced; tool outputs of PostToolUse
    /// events and checked file snapshots older than --days are dropped (replay
    /// and episodes keep working). Then the file is compacted. Needs a terminal.
    /// Shrink the ledger and redact what it holds.
    Prune {
        #[arg(long, default_value_t = 14)]
        days: i64,
    },
    /// The recorded hook events, oldest first. The way to read the ledger:
    /// an agent does not need to open hookwatch's state files itself.
    Events {
        /// Only sessions whose id starts with this.
        #[arg(long)]
        session: Option<String>,
        /// Only these events (comma separated), e.g. UserPromptSubmit,Stop.
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
        /// How many of the newest events to show (default: 50; with --json, all).
        #[arg(long)]
        last: Option<usize>,
        /// One JSON object per line: {ts_ms, session, kind, tool, payload}.
        /// This is what script jigs read; `kind` is the event as hookwatch
        /// classifies it (as in `stats`), and --kinds then filters on it.
        #[arg(long)]
        json: bool,
        /// With --json: only sessions that worked in this repo.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// With --json: only these tools (comma separated).
        #[arg(long, value_delimiter = ',')]
        tool: Vec<String>,
        /// With --json: only events at or before this time (a replayed jig
        /// sees the ledger as it was).
        #[arg(long)]
        until_ms: Option<i64>,
    },
    /// Frictions with what led to them (the human's goal, the agent's last
    /// tool calls): the material for writing a jig.
    Episodes {
        /// Print JSON (one array) for an AI to read.
        #[arg(long)]
        json: bool,
        /// How many of the newest episodes to show.
        #[arg(long, default_value_t = 20)]
        last: usize,
        /// How many tool calls before each friction to include.
        #[arg(long, default_value_t = 5)]
        context: usize,
        /// Only sessions that worked in this repo.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Run jigs over the recorded tool calls and show what they would have
    /// hit, and what happened to those calls. Checks a jig before it goes in.
    Replay {
        /// A jigs.json-shaped file of candidate jigs; only these are run.
        /// Without it, the jigs in effect are run.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Everything Jig Smith reads, as one JSON: episodes with the
    /// conversation around each, the jigs in effect, earlier proposals, and
    /// the places a written rule can go.
    SmithInput {
        /// How many of the newest episodes.
        #[arg(long, default_value_t = 20)]
        last: usize,
        /// The repo whose rule files are listed.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Check a Jig Smith proposal (shape, validation cases, replay) and
    /// store it. A failed gate stores it as rejected, with why.
    Propose {
        file: PathBuf,
        /// Run the checks and print the report; store nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// List proposals, or show one in full.
    Proposals { id: Option<String> },
    /// The human accepts an open proposal. A jig goes into jigs.json in
    /// shadow. Needs a terminal: an agent cannot accept its own proposal.
    Accept { id: String },
    /// The human rejects an open proposal, with why. Needs a terminal.
    Reject {
        id: String,
        #[arg(long)]
        why: String,
    },
    /// List the jigs in effect (built-in + jigs.json).
    Jigs,
    /// Put a jigs file in effect (copied to $HOOKWATCH_HOME/jigs.json, the
    /// old one backed up). Needs a terminal: it changes which jigs judge the
    /// agent, so the agent cannot run it.
    PutJigs { file: PathBuf },
    /// Print the hooks block to add to Claude Code's settings.
    Install,
}

fn home() -> PathBuf {
    match std::env::var_os("HOOKWATCH_HOME") {
        Some(h) => PathBuf::from(h),
        None => {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is not set")).join(".hookwatch")
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn load_jigs() -> anyhow::Result<Vec<Jig>> {
    let path = home().join("jigs.json");
    let file = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<JigFile>(&s)
            .with_context(|| format!("parse {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => JigFile::default(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    Ok(jig::merge(jig::builtin(), file))
}

fn ledger() -> anyhow::Result<Ledger> {
    Ledger::open(&home().join("ledger.db"))
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Hook => {
            let code = hook()?;
            std::process::exit(code);
        }
        Cmd::Why { id } => why(id),
        Cmd::Override {
            id,
            wrong,
            right: _,
            note,
        } => {
            // A label is the ground truth of a jig's precision: the human's.
            need_terminal("override")?;
            let l = ledger()?;
            let v = l.verdict_by_short(&id)?;
            let label = if wrong { "wrong" } else { "right" };
            l.add_label(&new_id(), now_ms(), &v.id, label, note.as_deref())?;
            println!("labelled {} ({}) as {label}", short(&v.id), v.jig);
            Ok(())
        }
        Cmd::Stats { repo } => stats(repo.as_deref()),
        Cmd::Status => {
            for line in status_lines(&ledger()?, true)? {
                println!("{line}");
            }
            Ok(())
        }
        Cmd::Prune { days } => prune(days),
        Cmd::Panel { repo } => panel(repo.as_deref()),
        Cmd::Events {
            session,
            kinds,
            last,
            json,
            repo,
            tool,
            until_ms,
        } => {
            if json {
                events_json(session, &kinds, last, repo.as_deref(), &tool, until_ms)
            } else {
                events(session, &kinds, last.unwrap_or(50) as i64)
            }
        }
        Cmd::Episodes {
            json,
            last,
            context,
            repo,
        } => episodes(json, last, context, repo.as_deref()),
        Cmd::Replay { file } => replay(file),
        Cmd::SmithInput { last, repo } => smith_input(last, &repo),
        Cmd::Propose { file, dry_run } => propose(&file, dry_run),
        Cmd::Proposals { id } => proposals(id),
        Cmd::Accept { id } => accept(&id),
        Cmd::PutJigs { file } => put_jigs(&file),
        Cmd::Reject { id, why } => {
            let by = decider("reject", &id)?;
            let l = ledger()?;
            let p = l.proposal_by_short(&id)?;
            l.decide_proposal(&p.id, "rejected", Some(&why), now_ms(), &by)?;
            println!("rejected {} ({}) by {by}", short(&p.id), p.intent);
            Ok(())
        }
        Cmd::Jigs => {
            for j in load_jigs()? {
                println!(
                    "{:<16} {:<8} {:<6} {}",
                    j.id,
                    j.lifecycle.as_str(),
                    j.action.as_str(),
                    j.description
                );
            }
            Ok(())
        }
        Cmd::Install => install(),
    }
}

/// Returns the exit code for Claude Code.
fn hook() -> anyhow::Result<i32> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let v: serde_json::Value = serde_json::from_str(&raw).context("hook input is not JSON")?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str());
    let event = field("hook_event_name").context("hook input has no hook_event_name")?;
    let session = field("session_id").context("hook input has no session_id")?;
    let tool = field("tool_name");

    let l = ledger()?;
    let ts = now_ms();
    let event_id = new_id();
    // Stored as it came; credentials are masked once per turn, at Stop
    // (one gitleaks run per turn instead of one per event).
    l.add_event(&EventRow {
        id: &event_id,
        ts_ms: ts,
        session,
        event,
        tool,
        payload: &raw,
    })?;

    match event {
        "Stop" => take_snapshots(&l, session, ts)?,
        "UserPromptSubmit" | "SessionEnd" => check_rework(&l, session, ts)?,
        _ => {}
    }
    // What the human (not the agent) is told this event: `systemMessage`.
    let mut for_human: Vec<String> = vec![];
    if event == "SessionStart" {
        // Only when something waits for them.
        let lines = status_lines(&l, false)?;
        if !lines.is_empty() {
            for_human.push(format!("hookwatch: {}", lines.join(" / ")));
        }
    }
    let mut granted = false;
    if event == "PreToolUse"
        && tool == Some("Bash")
        && let Some(cmd) = v.pointer("/tool_input/command").and_then(|c| c.as_str())
    {
        granted = note_grant(&l, &v, cmd, ts)?;
    }
    let call = if event == "PreToolUse" {
        let tool = tool.context("PreToolUse input has no tool_name")?;
        let input = v
            .get("tool_input")
            .context("PreToolUse input has no tool_input")?;
        let on_disk = if tool == "Write" {
            let p = input
                .get("file_path")
                .and_then(|x| x.as_str())
                .context("Write has no file_path")?;
            match std::fs::read_to_string(p) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e).with_context(|| format!("read {p}")),
            }
        } else {
            None
        };
        ToolCall::from_claude(tool, input, on_disk)?
    } else {
        ToolCall::for_event(event, tool, v.get("tool_input"))
    };
    let engine = Engine::new(load_jigs()?)?;
    jig::EVENT_TS_MS.store(ts, std::sync::atomic::Ordering::Relaxed);
    let evaluation = engine.evaluate_with(&call, &raw, &mut spawn_jig);
    for r in &evaluation.runs {
        l.add_run(&new_id(), ts, &event_id, r)?;
    }
    let once: std::collections::HashSet<String> = engine
        .jigs_all()
        .filter(|j| j.repeat == jig::Repeat::Once)
        .map(|j| j.id.clone())
        .collect();
    let mut hits = vec![];
    for h in evaluation.hits {
        // A standing state (the same evidence again) is said once.
        if once.contains(&h.jig_id) && l.has_verdict(&h.jig_id, &h.evidence)? {
            continue;
        }
        hits.push(h);
    }

    let mut block: Option<String> = None;
    let mut nudges: Vec<String> = vec![];
    for h in &hits {
        let id = new_id();
        let applied = h.lifecycle == Lifecycle::Enforce;
        l.add_verdict(&VerdictRow {
            id: &id,
            ts_ms: ts,
            event_id: &event_id,
            session,
            jig: &h.jig_id,
            action: h.action.as_str(),
            lifecycle: h.lifecycle.as_str(),
            applied,
            message: &h.message,
            evidence: &h.evidence,
        })?;
        if !applied {
            continue;
        }
        let text = format!(
            "[hookwatch:{}] {}\n  evidence: {}\n  verdict: {} (if this is wrong, the human runs `hookwatch override {} --wrong`)",
            h.jig_id,
            h.message,
            h.evidence,
            short(&id),
            short(&id)
        );
        match h.action {
            Action::Block if block.is_none() => block = Some(text),
            Action::Block => {}
            Action::Nudge => nudges.push(text),
        }
    }
    if let Some(text) = block {
        // Engine::new lets a block act on PreToolUse only.
        eprintln!("{text}");
        return Ok(2);
    }
    let mut out = serde_json::Map::new();
    let mut specific = serde_json::Map::new();
    if granted {
        // A block above returned already; deny / ask rules still win.
        specific.insert("permissionDecision".into(), "allow".into());
        specific.insert(
            "permissionDecisionReason".into(),
            "hookwatch: a role agent named in deciders.json decides this proposal (one-time grant)"
                .into(),
        );
    }
    if !nudges.is_empty() {
        match event {
            // These events take context for the agent.
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "UserPromptSubmit" => {
                specific.insert("additionalContext".into(), nudges.join("\n").into());
            }
            // A turn's end, a session's start: tell the human; the agent
            // is not made to go on.
            _ => for_human.extend(nudges),
        }
    }
    if !specific.is_empty() {
        specific.insert("hookEventName".into(), event.into());
        out.insert("hookSpecificOutput".into(), specific.into());
    }
    if !for_human.is_empty() {
        out.insert("systemMessage".into(), for_human.join("\n").into());
    }
    if !out.is_empty() {
        println!("{}", serde_json::Value::Object(out));
    }
    Ok(0)
}

fn events(session: Option<String>, kinds: &[String], last: i64) -> anyhow::Result<()> {
    let l = ledger()?;
    for e in l.recent_events(session.as_deref(), kinds, last)? {
        let p: serde_json::Value =
            serde_json::from_str(&e.payload).context("ledger payload is not JSON")?;
        println!(
            "{:>5} ago  {}  {:<18} {}",
            ago(e.ts_ms),
            &e.session[..e.session.len().min(8)],
            e.event,
            episode::summary(&e.event, e.tool.as_deref(), &p, 70)
        );
    }
    Ok(())
}

/// The ledger as JSON Lines, for script jigs. Oldest first.
fn events_json(
    session: Option<String>,
    kinds: &[String],
    last: Option<usize>,
    repo: Option<&std::path::Path>,
    tools: &[String],
    until_ms: Option<i64>,
) -> anyhow::Result<()> {
    let l = ledger()?;
    let (evs, _, _) = load_scoped(&l, repo)?;
    let keep: Vec<&episode::Ev> = evs
        .iter()
        .filter(|e| session.as_deref().is_none_or(|s| e.session.starts_with(s)))
        .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
        .filter(|e| tools.is_empty() || e.tool.as_ref().is_some_and(|t| tools.contains(t)))
        .filter(|e| until_ms.is_none_or(|u| e.ts_ms <= u))
        .collect();
    // load_scoped orders by session then time; a reader wants time.
    let mut keep = keep;
    keep.sort_by_key(|e| e.ts_ms);
    let skip = last.map_or(0, |n| keep.len().saturating_sub(n));
    for e in &keep[skip..] {
        println!(
            "{}",
            serde_json::json!({"ts_ms": e.ts_ms, "session": e.session, "kind": e.kind,
                   "tool": e.tool, "payload": e.payload})
        );
    }
    Ok(())
}

/// Sessions that worked in `repo`: any of their events has a cwd at or
/// under it. hookwatch is set up per repo, so one ledger holds many repos.
fn sessions_in(
    evs: &[episode::Ev],
    repo: &std::path::Path,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let root = repo
        .canonicalize()
        .with_context(|| format!("repo {} does not exist", repo.display()))?;
    let root = root.to_string_lossy().into_owned();
    let under = |cwd: &str| cwd == root || cwd.starts_with(&format!("{root}/"));
    Ok(evs
        .iter()
        .filter(|e| {
            e.payload
                .get("cwd")
                .and_then(|c| c.as_str())
                .is_some_and(under)
        })
        .map(|e| e.session.clone())
        .collect())
}

/// The events, reworks and labels of the sessions in `repo` (all when None).
fn load_scoped(
    l: &Ledger,
    repo: Option<&std::path::Path>,
) -> anyhow::Result<(
    Vec<episode::Ev>,
    Vec<ledger::ReworkRow>,
    Vec<ledger::LabelRow>,
)> {
    let evs = load_evs(l)?;
    let (reworks, labels) = (l.rework_rows()?, l.label_rows()?);
    let Some(repo) = repo else {
        return Ok((evs, reworks, labels));
    };
    let keep = sessions_in(&evs, repo)?;
    Ok((
        evs.into_iter()
            .filter(|e| keep.contains(&e.session))
            .collect(),
        reworks
            .into_iter()
            .filter(|r| keep.contains(&r.session))
            .collect(),
        labels
            .into_iter()
            .filter(|x| keep.contains(&x.session))
            .collect(),
    ))
}

/// What the human should know about the ledger. `always`: also when all is
/// well (the status command); otherwise only lines that ask for action.
fn status_lines(l: &Ledger, always: bool) -> anyhow::Result<Vec<String>> {
    const BIG: i64 = 50 * 1024 * 1024;
    const PRUNE_EVERY_MS: i64 = 14 * 86_400_000;
    let s = l.status()?;
    let file = std::fs::metadata(home().join("ledger.db"))
        .context("ledger.db")?
        .len() as i64;
    let mut out = vec![];
    if always {
        out.push(format!(
            "ledger: {} events, file {:.1} MB, last prune {}",
            s.events,
            file as f64 / 1048576.0,
            match s.last_prune_ms {
                Some(t) => format!("{} ago", ago(t)),
                None => "never".into(),
            }
        ));
    }
    let stale = match s.last_prune_ms {
        Some(t) => now_ms() - t > PRUNE_EVERY_MS,
        None => s.events > 0,
    };
    if file > BIG || (stale && file > BIG / 10) {
        out.push(format!(
            "ledger is {:.1} MB: run `hookwatch prune` in a terminal",
            file as f64 / 1048576.0
        ));
    }
    if s.open_proposals > 0 {
        out.push(format!(
            "{} open proposal(s): `hookwatch proposals`",
            s.open_proposals
        ));
    }
    if s.unlabelled_applied > 0 {
        out.push(format!(
            "{} verdict(s) acted on the agent without your label: `hookwatch why`, then `hookwatch override <id> --right|--wrong`",
            s.unlabelled_applied
        ));
    }
    Ok(out)
}

fn panel(repo: Option<&std::path::Path>) -> anyhow::Result<()> {
    use serde_json::json;
    let l = ledger()?;
    let s = l.status()?;
    let file = std::fs::metadata(home().join("ledger.db"))
        .context("ledger.db")?
        .len();
    let counts = l.jig_counts()?;
    let jigs: Vec<_> = load_jigs()?
        .iter()
        .map(|j| {
            let c = counts.iter().find(|c| c.jig == j.id);
            let n = |f: fn(&ledger::JigCount) -> i64| c.map(f).unwrap_or(0); // a jig that never fired
            json!({
                "id": j.id, "lifecycle": j.lifecycle.as_str(), "action": j.action.as_str(),
                "description": j.description,
                "fires": n(|c| c.fires), "applied": n(|c| c.applied),
                "wrong": n(|c| c.wrong), "right": n(|c| c.right),
            })
        })
        .collect();
    let proposals: Vec<_> = l
        .proposals()?
        .iter()
        .take(10)
        .map(|p| json!({"id": short(&p.id), "ts_ms": p.ts_ms, "status": p.status, "outlet": p.outlet, "intent": p.intent}))
        .collect();
    let verdicts: Vec<_> = l
        .recent_verdicts(10)?
        .iter()
        .map(|v| {
            json!({"id": short(&v.id), "ts_ms": v.ts_ms, "jig": v.jig, "action": v.action,
                   "applied": v.applied, "label": v.label, "evidence": v.evidence})
        })
        .collect();
    let (evs, reworks, _) = load_scoped(&l, repo)?;
    let stream: Vec<(String, i64, String)> = evs
        .into_iter()
        .map(|e| (e.session, e.ts_ms, e.kind))
        .collect();
    let f = friction::measure(&stream);
    let out = json!({
        "now_ms": now_ms(),
        "status": {
            "events": s.events, "file_bytes": file, "last_prune_ms": s.last_prune_ms,
            "open_proposals": s.open_proposals, "unlabelled_applied": s.unlabelled_applied,
            "attention": status_lines(&l, false)?,
        },
        "stopwatch": {
            "repo": repo.map(|r| r.display().to_string()),
            "sessions": f.sessions, "turns": f.turns, "cut_ins": f.cut_ins,
            "permission_wait_ms": f.wait_total_ms(), "question_wait_ms": f.question_waits_ms.iter().sum::<i64>(),
            "denied": f.denied, "tool_failures": f.tool_failures,
            "rework_files": reworks.len(),
            "rework_lines": reworks.iter().map(|r| r.added + r.removed).sum::<i64>(),
        },
        "jigs": jigs,
        "proposals": proposals,
        "verdicts": verdicts,
    });
    println!("{out}");
    Ok(())
}

fn prune(days: i64) -> anyhow::Result<()> {
    need_terminal("prune")?;
    let l = ledger()?;
    let path = home().join("ledger.db");
    let size = |p: &std::path::Path| -> anyhow::Result<u64> {
        Ok(std::fs::metadata(p).context("ledger.db")?.len())
    };
    let before = size(&path)?;
    // A copy first: pruning drops data, and a bug in it must not cost the ledger.
    let bak = path.with_extension(format!("db.bak.{}", now_ms()));
    l.backup_to(&bak)?;
    println!("backup: {}", bak.display());
    let texts = l.all_texts()?;
    let secrets = mask::find_secrets(&texts.iter().map(String::as_str).collect::<Vec<_>>())?;
    let now = now_ms();
    let r = l.prune(now - days * 86_400_000, now, &|t: &str| {
        mask::mask(t, &secrets)
    })?;
    l.vacuum()?;
    println!(
        "masked {} rows ({} secrets found by gitleaks), repaired {} broken rows, dropped {} tool outputs and {} snapshots older than {days} days",
        r.redacted,
        secrets.len(),
        r.repaired,
        r.outputs_dropped,
        r.snapshots_emptied
    );
    println!(
        "stored text {:.1} → {:.1} MB, file {:.1} → {:.1} MB",
        r.bytes_before as f64 / 1048576.0,
        r.bytes_after as f64 / 1048576.0,
        before as f64 / 1048576.0,
        size(&path)? as f64 / 1048576.0
    );
    Ok(())
}

/// Who may decide what. Kept by the human in $HOOKWATCH_HOME/deciders.json:
/// `{"accept": ["hookwatch:jig-judge"], "reject": ["hookwatch:jig-judge"]}`
/// (agent types, as Claude Code names them in the hook input). Ops not
/// listed, and labels, put-jigs and prune, are the human's only.
#[derive(Debug, Default, serde::Deserialize)]
struct Deciders {
    #[serde(default)]
    accept: Vec<String>,
    #[serde(default)]
    reject: Vec<String>,
}

fn deciders() -> anyhow::Result<Deciders> {
    let p = home().join("deciders.json");
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parse {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Deciders::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

/// How long a grant waits for its command (the hook runs just before it).
const GRANT_MS: i64 = 60_000;

/// At PreToolUse: when a role agent named in deciders.json runs
/// `hookwatch accept|reject <id>`, leave it a one-time grant. The agent type
/// comes from Claude Code's hook input; the main agent has none.
/// Returns true when it left a grant: the hook then lets this one call
/// through the permission check (the role agent's decision is hookwatch's
/// to allow, not the auto mode classifier's).
fn note_grant(l: &Ledger, v: &serde_json::Value, cmd: &str, ts: i64) -> anyhow::Result<bool> {
    let Some((op, target)) = deciding_command(cmd) else {
        return Ok(false);
    };
    let Some(agent_type) = v.get("agent_type").and_then(|x| x.as_str()) else {
        return Ok(false); // the main agent: no role
    };
    let d = deciders()?;
    let allowed = match op.as_str() {
        "accept" => &d.accept,
        _ => &d.reject,
    };
    if !allowed.iter().any(|a| a == agent_type) {
        return Ok(false);
    }
    let agent_id = v.get("agent_id").and_then(|x| x.as_str());
    l.add_grant(&new_id(), ts, &op, &target, agent_type, agent_id)?;
    Ok(true)
}

/// (op, proposal id) when the whole command is `hookwatch accept|reject <id>`.
fn deciding_command(cmd: &str) -> Option<(String, String)> {
    use std::sync::LazyLock;
    // The whole command must be the deciding command, nothing chained: the
    // hook allows it. A reason is quoted text without $, ` or \ (no
    // expansion in the shell).
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"^\s*hookwatch\s+(accept|reject)\s+([0-9a-f-]+)(?:\s+--why\s+(?:'[^']*'|"[^"$`\\]*"))?\s*$"#,
        )
        .unwrap()
    });
    let c = RE.captures(cmd)?;
    Some((c[1].to_owned(), c[2].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::deciding_command;

    #[test]
    fn only_the_bare_deciding_command_counts() {
        let ok = |c: &str| deciding_command(c).is_some();
        assert_eq!(
            deciding_command("hookwatch accept 479a7f4d"),
            Some(("accept".into(), "479a7f4d".into()))
        );
        assert!(ok(
            r#"hookwatch reject 479a7f4d --why "gate cases are one-sided""#
        ));
        assert!(ok("hookwatch reject 479a7f4d --why 'x; y'"));
        for bad in [
            "hookwatch accept 479a7f4d; rm -rf x",
            "hookwatch accept 479a7f4d && echo",
            "cd /r && hookwatch accept 479a7f4d",
            r#"hookwatch reject 479a7f4d --why "$(touch x)""#,
            "hookwatch reject 479a7f4d --why \"a\" ; ls",
            "/tmp/evil/hookwatch accept 479a7f4d",
            "hookwatch override 479a7f4d --right",
        ] {
            assert!(!ok(bad), "{bad}");
        }
    }
}

/// Who runs this deciding command: "human" at a terminal, else
/// "agent:<type>" when the hook left that role agent a grant for it.
fn decider(op: &str, target: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Ok("human".into());
    }
    // Do not create a ledger for an agent that has no grant anyway.
    if !home().join("ledger.db").exists() {
        anyhow::bail!("{op} needs a terminal, or a role agent with a grant (deciders.json)");
    }
    match ledger()?.take_grant(op, target, now_ms(), GRANT_MS)? {
        Some(t) => Ok(format!("agent:{t}")),
        None => {
            anyhow::bail!("{op} needs a terminal, or a role agent with a grant (deciders.json)")
        }
    }
}

/// Where the scripts of jigs in effect live: under hookwatch's own state,
/// which tamper-guard keeps the agent from writing.
fn jig_dir(id: &str) -> PathBuf {
    home().join("jigs").join(id)
}

/// The script files a command jig names (argv entries that are files).
fn script_args(argv: &[String]) -> anyhow::Result<Vec<(usize, PathBuf)>> {
    let h = std::env::var("HOME").context("HOME is not set")?;
    Ok(argv
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, a)| !a.starts_with('-'))
        .map(|(i, a)| (i, PathBuf::from(jig::expand_home(a, &h))))
        .filter(|(_, p)| p.is_file())
        .collect())
}

/// Read a command jig's scripts and point its argv at their protected
/// place: (file name → text). The caller writes them with `place_scripts`.
fn take_scripts(jig: &mut jig::Jig) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    let jig::Rule::Command { argv, .. } = &mut jig.rule else {
        return Ok(out);
    };
    for (i, p) in script_args(argv)? {
        let name = p
            .file_name()
            .context("script path has no file name")?
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        out.insert(name.clone(), text);
        argv[i] = home_arg(&jig_dir(&jig.id).join(&name))?;
    }
    Ok(out)
}

/// `path` as an argv entry: `$HOME/...` when it is under $HOME (works on
/// any machine), else as it is (a HOOKWATCH_HOME elsewhere).
fn home_arg(path: &std::path::Path) -> anyhow::Result<String> {
    let h = std::env::var("HOME").context("HOME is not set")?;
    Ok(match path.strip_prefix(&h) {
        Ok(rel) => format!("$HOME/{}", rel.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    })
}

fn place_scripts(
    id: &str,
    scripts: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }
    let dir = jig_dir(id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (name, text) in scripts {
        let p = dir.join(name);
        std::fs::write(&p, text).with_context(|| format!("write {}", p.display()))?;
        println!("placed {}", p.display());
    }
    Ok(())
}

/// Command jigs in effect may only run scripts from their protected place;
/// a candidate (replay --file, the propose gate) may run any.
static ANY_SCRIPT_PLACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Every recorded event, classified, ordered by session then time.
fn load_evs(l: &Ledger) -> anyhow::Result<Vec<episode::Ev>> {
    let mut out = vec![];
    for e in l.event_stream()? {
        let payload: serde_json::Value =
            serde_json::from_str(&e.payload).context("ledger payload is not JSON")?;
        let kind = friction::classify(&e.event, e.tool.as_deref(), &payload);
        out.push(episode::Ev {
            session: e.session,
            ts_ms: e.ts_ms,
            kind,
            tool: e.tool,
            payload,
        });
    }
    Ok(out)
}

fn episodes(
    json: bool,
    last: usize,
    context: usize,
    repo: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let l = ledger()?;
    let (evs, reworks, labels) = load_scoped(&l, repo)?;
    let all = episode::episodes(&evs, &reworks, &labels, context);
    let eps = &all[all.len().saturating_sub(last)..];
    if json {
        println!("{}", serde_json::to_string_pretty(eps)?);
        return Ok(());
    }
    for e in eps {
        println!(
            "[{}] {} ago  session {}",
            e.kind,
            ago(e.ts_ms),
            &e.session[..e.session.len().min(8)]
        );
        if let Some(g) = &e.goal {
            let g: String = g.replace('\n', " ").chars().take(100).collect();
            println!("  goal:   {g}");
        }
        for r in &e.recent {
            println!("  before: {r}");
        }
        let detail = serde_json::to_string(&e.detail)?;
        let detail: String = detail.chars().take(300).collect();
        println!("  detail: {detail}");
        println!();
    }
    Ok(())
}

fn replay(file: Option<PathBuf>) -> anyhow::Result<()> {
    let jigs = match &file {
        Some(p) => {
            let s = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            let f: JigFile =
                serde_json::from_str(&s).with_context(|| format!("parse {}", p.display()))?;
            // A candidate's scripts are still in the repo.
            ANY_SCRIPT_PLACE.store(true, std::sync::atomic::Ordering::Relaxed);
            f.jigs
        }
        None => load_jigs()?,
    };
    let engine = Engine::new(jigs)?;
    let l = ledger()?;
    let r = replay::run(&engine, &load_evs(&l)?, &mut spawn_jig);
    println!(
        "replayed {} tool calls ({} could not be read: {:?})",
        r.calls,
        r.unreadable.len(),
        r.unreadable.first()
    );
    if r.command_runs > 0 {
        println!(
            "command jigs ran {} times, {} failed{}",
            r.command_runs,
            r.command_errors.len(),
            match r.command_errors.first() {
                Some(e) => format!(" (first: {e})"),
                None => String::new(),
            }
        );
    }
    println!("Write calls are judged as new files: the content on disk then is not recorded.");
    println!();
    println!(
        "{:<16} {:>4} {:>5} {:>6} {:>6} {:>7} {:>7}",
        "jig", "hits", "ran", "denied", "failed", "cut-in", "no-post"
    );
    for j in &r.jigs {
        let c = |o| j.outcomes.get(o).copied().unwrap_or(0); // an outcome not seen for this jig
        println!(
            "{:<16} {:>4} {:>5} {:>6} {:>6} {:>7} {:>7}",
            j.jig,
            j.hits,
            c("ran"),
            c("denied"),
            c("failed"),
            c("cut_in"),
            c("no_post")
        );
        for s in &j.samples {
            println!("    {s}");
        }
    }
    println!();
    println!(
        "ran = the call ran and nothing stopped it: a block there would have stopped normal work."
    );
    Ok(())
}

/// Accepting and rejecting are the human's. Claude Code runs Bash without a
/// terminal on stdin, so this stops an agent deciding its own proposal.
fn need_terminal(what: &str) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("{what} needs a terminal: the human runs it, not an agent");
    }
    Ok(())
}

/// Files under `repo` a written rule can go in: *.md at the root and in
/// docs/, and skills/*/SKILL.md.
fn rule_places(repo: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut out = vec![];
    let mut md_in = |dir: PathBuf, deep: bool| -> anyhow::Result<()> {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()), // the repo has no such dir
            Err(e) => return Err(e).with_context(|| format!("list {}", dir.display())),
        };
        for e in entries {
            let p = e?.path();
            if deep {
                let skill = p.join("SKILL.md");
                if skill.is_file() {
                    out.push(skill.display().to_string());
                }
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p.display().to_string());
            }
        }
        Ok(())
    };
    md_in(repo.to_path_buf(), false)?;
    md_in(repo.join("docs"), false)?;
    md_in(repo.join("skills"), true)?;
    md_in(repo.join("plugin").join("skills"), true)?;
    out.sort();
    Ok(out)
}

fn smith_input(last: usize, repo: &std::path::Path) -> anyhow::Result<()> {
    let l = ledger()?;
    // Smith works on one repo: its sessions, and its rule files.
    let (evs, reworks, labels) = load_scoped(&l, Some(repo))?;
    let all = episode::episodes(&evs, &reworks, &labels, 5);
    let eps = &all[all.len().saturating_sub(last)..];
    let mut transcripts: std::collections::HashMap<String, Option<String>> = Default::default();
    let mut out_eps = vec![];
    for e in eps {
        let mut v = serde_json::to_value(e)?;
        if e.kind == "rework" {
            // The agent's last write to the file the human then changed: a
            // validation case copies it exactly, so replay can confirm the hit.
            let path = e.detail.get("path").and_then(|p| p.as_str());
            let src = evs.iter().rev().find(|x| {
                x.session == e.session
                    && x.ts_ms < e.ts_ms
                    && x.kind == "PreToolUse"
                    && x.payload
                        .pointer("/tool_input/file_path")
                        .and_then(|p| p.as_str())
                        == path
            });
            if let Some(x) = src {
                v["source_call"] =
                    serde_json::json!({"tool_name": x.tool, "tool_input": x.payload["tool_input"]});
            }
        }
        if let Some(w) = &e.transcript {
            if !transcripts.contains_key(&w.path) {
                let body = read_if_exists(&w.path)?; // None: the transcript was deleted
                transcripts.insert(w.path.clone(), body);
            }
            v["conversation"] = match &transcripts[&w.path] {
                Some(body) => serde_json::to_value(smith::excerpt(body.lines(), &w.from, &w.to)?)?,
                None => serde_json::json!({ "missing": w.path }),
            };
        }
        out_eps.push(v);
    }
    let props: Vec<serde_json::Value> = l
        .proposals()?
        .iter()
        .map(|p| serde_json::json!({"id": p.id, "status": p.status, "outlet": p.outlet, "intent": p.intent, "why": p.why}))
        .collect();
    let out = serde_json::json!({
        "episodes": out_eps,
        "jigs": load_jigs()?,
        "proposals": props,
        "rule_places": rule_places(repo)?,
        "contract": "Write one proposal JSON (see docs/jig-smith.md) and run `hookwatch propose <file>`.",
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn outlet_name(c: &proposal::Candidate) -> &'static str {
    match c {
        proposal::Candidate::Jig { .. } => "jig",
        proposal::Candidate::HookL2Pending { .. } => "l2_pending",
        proposal::Candidate::Rule { .. } => "rule",
        proposal::Candidate::Nothing => "nothing",
    }
}

/// Run each case through the candidate jig alone.
fn run_cases(
    engine: &Engine,
    cases: &[proposal::Case],
) -> Vec<(proposal::Case, proposal::CaseResult)> {
    use proposal::CaseResult;
    cases
        .iter()
        .map(|c| {
            let got = match ToolCall::from_claude(&c.tool_name, &c.tool_input, None) {
                Err(e) => CaseResult::Error(format!("{e:#}")),
                Ok(call) => {
                    let payload = serde_json::json!({
                        "hook_event_name": "PreToolUse", "tool_name": c.tool_name, "tool_input": c.tool_input
                    })
                    .to_string();
                    let ev = engine.evaluate_with(&call, &payload, &mut spawn_jig);
                    match ev.runs.iter().find_map(|r| match &r.outcome {
                        jig::RunOutcome::Error(m) => Some(m.clone()),
                        _ => None,
                    }) {
                        Some(m) => CaseResult::Error(m),
                        None if ev.hits.is_empty() => CaseResult::Pass,
                        None => CaseResult::Hit,
                    }
                }
            };
            (c.clone(), got)
        })
        .collect()
}

fn propose(file: &std::path::Path, dry_run: bool) -> anyhow::Result<()> {
    let s = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let d: proposal::Draft =
        serde_json::from_str(&s).with_context(|| format!("parse {}", file.display()))?;
    let shape = proposal::check_shape(&d);
    if !shape.is_empty() {
        anyhow::bail!(
            "the proposal is not well formed (not stored):\n  {}",
            shape.join("\n  ")
        );
    }
    let l = ledger()?;
    // A candidate runs its scripts where they are (in the repo).
    ANY_SCRIPT_PLACE.store(true, std::sync::atomic::Ordering::Relaxed);
    let gate = match &d.candidate {
        proposal::Candidate::Jig { jig } => {
            let engine = Engine::new(vec![jig.clone()])?;
            let cases = run_cases(&engine, &d.cases);
            let r = replay::run(&engine, &load_evs(&l)?, &mut spawn_jig);
            // A hit is confirmed by a hit case, or by a human's right label.
            let right = l.right_labelled_calls()?;
            let hits: Vec<proposal::ReplayHit> = r
                .jigs
                .into_iter()
                .flat_map(|j| j.all)
                .map(|(out, tool, input, evidence)| proposal::ReplayHit {
                    ran: out == "ran",
                    confirmed: d.cases.iter().any(|c| {
                        c.expect == proposal::Expect::Hit
                            && c.tool_name == tool
                            && c.tool_input == input
                    }) || right.iter().any(|(t, i)| *t == tool && *i == input),
                    evidence,
                })
                .collect();
            Some(proposal::gate(jig.action, &cases, &hits))
        }
        _ => None, // rules and nothing are judged by the human alone
    };
    if let Some(g) = &gate {
        println!(
            "gate: {} (cases {}/{}, replay hits {}, unconfirmed {})",
            if g.passed { "passed" } else { "FAILED" },
            g.cases_ok,
            g.cases,
            g.replay_hits,
            g.unconfirmed
        );
        for f in &g.failures {
            println!("  - {f}");
        }
        for s in &g.unconfirmed_samples {
            println!("  unconfirmed: {s}");
        }
    }
    if dry_run {
        println!("dry run: nothing stored");
        return Ok(());
    }
    // Keep the scripts the gate just ran, and point the jig at the place
    // accept will write them to.
    let mut d = d;
    if let proposal::Candidate::Jig { jig } = &mut d.candidate {
        d.scripts = take_scripts(jig)?;
    }
    let s = serde_json::to_string_pretty(&d)?;
    let id = new_id();
    let gate_json = gate.as_ref().map(serde_json::to_string).transpose()?;
    let rejected = gate
        .as_ref()
        .filter(|g| !g.passed)
        .map(|g| g.failures.join("; "));
    l.add_proposal(
        &id,
        now_ms(),
        &d.intent,
        outlet_name(&d.candidate),
        &s,
        gate_json.as_deref(),
        rejected.as_deref(),
    )?;
    match rejected {
        Some(_) => println!("stored {} as rejected by the gate", short(&id)),
        None => println!(
            "stored {} as open: the human runs `hookwatch proposals {}`",
            short(&id),
            short(&id)
        ),
    }
    Ok(())
}

fn proposals(id: Option<String>) -> anyhow::Result<()> {
    let l = ledger()?;
    match id {
        None => {
            for p in l.proposals()? {
                println!(
                    "{}  {:>4} ago  {:<9} {:<10} {}",
                    short(&p.id),
                    ago(p.ts_ms),
                    p.status,
                    p.outlet,
                    p.intent
                );
            }
        }
        Some(id) => {
            let p = l.proposal_by_short(&id)?;
            println!("proposal  {} ({}, {} ago)", p.id, p.status, ago(p.ts_ms));
            if let Some(by) = &p.decided_by {
                println!("decided   by {by}: {}", p.why.as_deref().unwrap_or("-")); // accepted carries no why
            }
            let d: serde_json::Value =
                serde_json::from_str(&p.draft).context("stored draft is not JSON")?;
            println!("{}", serde_json::to_string_pretty(&d)?);
            if let Some(g) = &p.gate {
                let g: serde_json::Value =
                    serde_json::from_str(g).context("stored gate is not JSON")?;
                println!("gate {}", serde_json::to_string_pretty(&g)?);
            }
        }
    }
    Ok(())
}

fn put_jigs(file: &std::path::Path) -> anyhow::Result<()> {
    need_terminal("put-jigs")?;
    let s = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut parsed: JigFile = serde_json::from_str(&s).with_context(|| {
        format!(
            "parse {} (`test_weakening` is now the script jig jigs/tests_weakened.py)",
            file.display()
        )
    })?;
    // Copy each command jig's scripts to their protected place and point
    // argv there: the repo's copy can then change without changing a judge.
    let mut placed = vec![];
    for j in &mut parsed.jigs {
        let scripts = take_scripts(j)?;
        placed.push((j.id.clone(), scripts));
    }
    // Refuse a file the hook could not load: bad regexes, empty argv.
    Engine::new(jig::merge(
        jig::builtin(),
        serde_json::from_value(serde_json::to_value(&parsed)?)?,
    ))?;
    for (id, scripts) in &placed {
        place_scripts(id, scripts)?;
    }
    let s = serde_json::to_string_pretty(&parsed)? + "\n";
    let dest = home().join("jigs.json");
    std::fs::create_dir_all(home()).with_context(|| format!("create {}", home().display()))?;
    match std::fs::read_to_string(&dest) {
        Ok(old) if old == s => {
            println!("{} is already the same", dest.display());
            return Ok(());
        }
        Ok(_) => {
            let bak = dest.with_extension(format!("json.bak.{}", now_ms()));
            std::fs::copy(&dest, &bak).with_context(|| format!("back up to {}", bak.display()))?;
            println!("backup: {}", bak.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // the first jigs file
        Err(e) => return Err(e).with_context(|| format!("read {}", dest.display())),
    }
    std::fs::write(&dest, &s).with_context(|| format!("write {}", dest.display()))?;
    println!("installed: {}", dest.display());
    for j in load_jigs()? {
        println!(
            "{:<18} {:<8} {:<6} {}",
            j.id,
            j.lifecycle.as_str(),
            j.action.as_str(),
            j.description
        );
    }
    Ok(())
}

fn accept(id: &str) -> anyhow::Result<()> {
    let by = decider("accept", id)?;
    let l = ledger()?;
    let p = l.proposal_by_short(id)?;
    let d: proposal::Draft =
        serde_json::from_str(&p.draft).context("stored draft is not a proposal")?;
    match &d.candidate {
        proposal::Candidate::Jig { jig } => {
            let path = home().join("jigs.json");
            let mut file = match std::fs::read_to_string(&path) {
                Ok(s) => serde_json::from_str::<JigFile>(&s)
                    .with_context(|| format!("parse {}", path.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => JigFile::default(),
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            };
            let in_effect = load_jigs()?;
            if in_effect.iter().any(|j| j.id == jig.id) {
                anyhow::bail!("a jig with id {} is already in effect", jig.id);
            }
            if let Some(old) = &d.replaces {
                if !in_effect.iter().any(|j| j.id == *old) {
                    anyhow::bail!("replaces {old}, but no such jig is in effect");
                }
                file.lifecycle.insert(old.clone(), jig::Lifecycle::Retired);
                println!("retired jig {old}");
            }
            if path.exists() {
                let bak = path.with_extension(format!("json.bak.{}", now_ms()));
                std::fs::copy(&path, &bak)
                    .with_context(|| format!("back up to {}", bak.display()))?;
                println!("backup: {}", bak.display());
            }
            // The scripts as the gate ran them, not as the repo has them now.
            place_scripts(&jig.id, &d.scripts)?;
            file.jigs.push(jig.clone());
            std::fs::write(&path, serde_json::to_string_pretty(&file)? + "\n")
                .with_context(|| format!("write {}", path.display()))?;
            println!("added jig {} to {} in shadow", jig.id, path.display());
        }
        proposal::Candidate::Rule { file, text } => {
            println!("apply this rule yourself, in {file}:\n\n{text}\n");
        }
        proposal::Candidate::HookL2Pending { question } => {
            println!("kept for the L2 judge (not installed): {question}");
        }
        proposal::Candidate::Nothing => println!("nothing to install"),
    }
    l.decide_proposal(&p.id, "accepted", None, now_ms(), &by)?;
    println!("accepted {} by {by}", short(&p.id));
    Ok(())
}

/// Run a command jig as a child: payload on stdin, killed at the timeout.
/// stdout is not read by hookwatch (a jig speaks through exit and stderr).
fn spawn_jig(argv: &[String], payload: &str, timeout_ms: u64) -> anyhow::Result<jig::RunResult> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let home = std::env::var("HOME").context("HOME is not set; needed to expand $HOME/ in argv")?;
    if !ANY_SCRIPT_PLACE.load(std::sync::atomic::Ordering::Relaxed) {
        // A jig in effect runs only scripts put in place by a decider, so
        // the agent cannot change a judge by editing the repo's copy.
        let place = crate::home().join("jigs");
        for (_, p) in script_args(argv)? {
            if !p.starts_with(&place) {
                anyhow::bail!(
                    "script {} is outside {}: put the jigs file again (just jigs)",
                    p.display(),
                    place.display()
                );
            }
        }
    }
    let argv: Vec<String> = argv.iter().map(|a| jig::expand_home(a, &home)).collect();
    // Python exits 2 when it cannot open the script, which reads as a hit:
    // a missing file must be the jig failing, not a verdict (2026-11-14).
    for a in argv
        .iter()
        .filter(|a| a.contains('/') && !a.starts_with('-'))
    {
        if !std::path::Path::new(a).exists() {
            anyhow::bail!("argv path does not exist: {a}");
        }
    }
    let start = std::time::Instant::now();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        // A jig that reads the ledger calls this binary, as of this event.
        .env(
            "HOOKWATCH_BIN",
            std::env::current_exe().context("own path")?,
        )
        .env(
            "HOOKWATCH_EVENT_TS_MS",
            jig::EVENT_TS_MS
                .load(std::sync::atomic::Ordering::Relaxed)
                .to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {}", argv[0]))?;
    // Feed stdin and drain stderr on threads, so a full pipe cannot stall us.
    let mut stdin = child.stdin.take().context("child has no stdin")?;
    let input = payload.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let mut stderr = child.stderr.take().context("child has no stderr")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        stderr.read_to_string(&mut s).map(|_| s)
    });
    let deadline = std::time::Duration::from_millis(timeout_ms);
    let exit = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status.code().context("jig was ended by a signal")?);
        }
        if start.elapsed() >= deadline {
            child.kill()?;
            child.wait()?;
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    let ms = start.elapsed().as_millis() as u64;
    match writer.join() {
        Ok(Ok(())) => {}
        // A jig may exit without reading all of stdin; that is its choice.
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Ok(Err(e)) => return Err(e).context("write payload to jig"),
        Err(_) => anyhow::bail!("stdin writer thread panicked"),
    }
    let stderr = match reader.join() {
        Ok(r) => r.context("read jig stderr")?,
        Err(_) => anyhow::bail!("stderr reader thread panicked"),
    };
    Ok(jig::RunResult { exit, stderr, ms })
}

/// `Ok(None)` when the file is not there (deleted); other read errors stop.
fn read_if_exists(path: &str) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {path}")),
    }
}

/// At Stop: keep the content of every file the agent edited in this turn,
/// and mask credentials in the turn's events and those snapshots (one
/// gitleaks run for the whole turn).
fn take_snapshots(l: &Ledger, session: &str, ts: i64) -> anyhow::Result<()> {
    let mut paths = std::collections::BTreeSet::new();
    for p in l.post_tool_payloads_this_turn(session)? {
        let v: serde_json::Value =
            serde_json::from_str(&p).context("ledger payload is not JSON")?;
        if let Some(path) = rework::edited_path(&v)? {
            paths.insert(path);
        }
    }
    let mut files = vec![];
    for path in paths {
        // A file the agent deleted has nothing to compare later.
        if let Some(content) = read_if_exists(&path)? {
            files.push((path, content));
        }
    }
    let turn = l.turn_rows(session)?;
    let texts: Vec<&str> = turn
        .iter()
        .map(|(_, p)| p.as_str())
        .chain(files.iter().map(|(_, c)| c.as_str()))
        .collect();
    let secrets = mask::find_secrets(&texts)?;
    for (id, payload) in &turn {
        let (masked, n) = mask::mask(payload, &secrets);
        if n > 0 {
            l.set_payload(id, &masked)?;
        }
    }
    for (path, content) in files {
        let (content, _) = mask::mask(&content, &secrets);
        l.put_snapshot(&new_id(), session, &path, ts, &content)?;
    }
    Ok(())
}

/// At the next prompt (or session end): what did the human change since?
fn check_rework(l: &Ledger, session: &str, ts: i64) -> anyhow::Result<()> {
    let snaps = l.open_snapshots(session)?;
    let mut nows = vec![];
    for snap in &snaps {
        nows.push(match read_if_exists(&snap.path)? {
            Some(s) => s,
            None => String::new(), // the human deleted it: every line removed
        });
    }
    // The snapshots are stored masked; mask the files now the same way, so
    // the diff compares like with like and carries no credential.
    let secrets = mask::find_secrets(&nows.iter().map(String::as_str).collect::<Vec<_>>())?;
    for (snap, now) in snaps.into_iter().zip(nows) {
        let (now, _) = mask::mask(&now, &secrets);
        let diff = rework::diff_text(&snap.content, &now);
        // A pure move of lines has a diff but +0 -0; it is still the human's change.
        let found = if diff.is_empty() {
            None
        } else {
            let (added, removed) = rework::line_delta(&snap.content, &now);
            Some(ledger::ReworkFound {
                id: new_id(),
                ts_ms: ts,
                added,
                removed,
                diff,
            })
        };
        l.close_snapshot(&snap, session, found)?;
    }
    Ok(())
}

fn short(id: &str) -> &str {
    // UUID v7 starts with the time; the tail is the random part.
    &id[id.len() - 8..]
}

fn ago(ts_ms: i64) -> String {
    let s = (now_ms() - ts_ms) / 1000;
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

fn why(id: Option<String>) -> anyhow::Result<()> {
    let l = ledger()?;
    match id {
        None => {
            let vs = l.recent_verdicts(20)?;
            if vs.is_empty() {
                println!("no verdicts yet");
            }
            for v in vs {
                let what = if v.applied {
                    v.action.clone()
                } else {
                    format!("would {}", v.action)
                };
                println!(
                    "{}  {:>4} ago  {:<16} {:<13} {:<6} {}",
                    short(&v.id),
                    ago(v.ts_ms),
                    v.jig,
                    what,
                    v.label.as_deref().unwrap_or("-"),
                    v.evidence
                );
            }
        }
        Some(id) => {
            let v = l.verdict_by_short(&id)?;
            println!("verdict   {}", v.id);
            println!("when      {} ago, session {}", ago(v.ts_ms), v.session);
            println!("jig       {} ({}, {})", v.jig, v.lifecycle, v.action);
            println!("applied   {}", v.applied);
            println!("evidence  {}", v.evidence);
            println!("message   {}", v.message);
            println!("label     {}", v.label.as_deref().unwrap_or("-"));
            let payload = l
                .event_payload(&v.event_id)?
                .with_context(|| format!("event {} of the verdict is missing", v.event_id))?;
            let p: serde_json::Value = serde_json::from_str(&payload)?;
            println!(
                "tool      {}",
                p.get("tool_name")
                    .and_then(|x| x.as_str())
                    .context("ledger payload of the verdict has no tool_name")?
            );
            println!(
                "input     {}",
                serde_json::to_string_pretty(&p["tool_input"])?
            );
        }
    }
    Ok(())
}

fn stats(repo: Option<&std::path::Path>) -> anyhow::Result<()> {
    let l = ledger()?;
    let (evs, reworks, _) = load_scoped(&l, repo)?;
    let stream: Vec<(String, i64, String)> = evs
        .into_iter()
        .map(|e| (e.session, e.ts_ms, e.kind))
        .collect();
    let f = friction::measure(&stream);
    let ms = |m: Option<i64>| {
        m.map(|m| format!("{:.1}s", m as f64 / 1000.0))
            .unwrap_or("-".into())
    };
    println!("== stopwatch ==");
    println!("sessions             {}", f.sessions);
    println!(
        "turns                {} (cut-ins {}: Esc or a mid-turn message), agent messages {}",
        f.turns, f.cut_ins, f.agent_messages
    );
    let total = |w: &[i64]| w.iter().sum::<i64>();
    println!(
        "question waits       {} (total {})",
        f.question_waits_ms.len(),
        ms(Some(total(&f.question_waits_ms)))
    );
    println!(
        "permission waits     {} (total {}, p50 {}, max {})",
        f.permission_requests,
        ms(Some(f.wait_total_ms())),
        ms(f.wait_p50_ms()),
        ms(f.wait_max_ms())
    );
    println!("permission denied    {}", f.denied);
    println!("tool failures        {}", f.tool_failures);
    println!("stop failures        {}", f.stop_failures);
    let stops = f.by_event.get("Stop").copied().unwrap_or(0); // no Stop event yet
    // One check (one prompt) closes every file of a turn at the same time.
    let turns: std::collections::HashSet<(&str, i64)> = reworks
        .iter()
        .map(|r| (r.session.as_str(), r.ts_ms))
        .collect();
    println!(
        "rework               {} of {} stopped turns, {} files, +{} -{} lines",
        turns.len(),
        stops,
        reworks.len(),
        reworks.iter().map(|r| r.added).sum::<i64>(),
        reworks.iter().map(|r| r.removed).sum::<i64>()
    );
    for r in reworks.iter().rev().take(5) {
        println!(
            "  {:>4} ago  +{} -{}  {}",
            ago(r.ts_ms),
            r.added,
            r.removed,
            r.path
        );
    }
    println!(
        "events               {}",
        f.by_event
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!();
    println!("== jigs ==");
    println!(
        "{:<16} {:>5} {:>7} {:>5} {:>5} {:>9}",
        "jig", "fires", "applied", "wrong", "right", "precision"
    );
    for c in l.jig_counts()? {
        let labelled = c.wrong + c.right;
        let p = if labelled == 0 {
            "-".into()
        } else {
            format!("{:.0}%", 100.0 * c.right as f64 / labelled as f64)
        };
        println!(
            "{:<16} {:>5} {:>7} {:>5} {:>5} {:>9}",
            c.jig, c.fires, c.applied, c.wrong, c.right, p
        );
    }
    let runs = l.run_stats()?;
    if !runs.is_empty() {
        println!();
        println!("== command jigs (hookwatch runs them) ==");
        println!(
            "{:<16} {:>5} {:>5} {:>6} {:>7} {:>7}",
            "jig", "runs", "hits", "errors", "p50 ms", "max ms"
        );
        for s in runs {
            // ms is sorted and has one entry per run, so it is not empty here.
            let p50 = s.ms[s.ms.len() / 2];
            let max = s.ms[s.ms.len() - 1];
            println!(
                "{:<16} {:>5} {:>5} {:>6} {:>7} {:>7}",
                s.jig, s.runs, s.hits, s.errors, p50, max
            );
        }
    }
    Ok(())
}

fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let cmd = format!("{} hook", exe.display());
    let entry = serde_json::json!([{"matcher": "*", "hooks": [{"type": "command", "command": cmd, "timeout": 5}]}]);
    let plain = serde_json::json!([{"hooks": [{"type": "command", "command": cmd, "timeout": 5}]}]);
    let mut hooks = serde_json::Map::new();
    for ev in [
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "PermissionRequest",
        "PermissionDenied",
    ] {
        hooks.insert(ev.into(), entry.clone());
    }
    for ev in [
        "UserPromptSubmit",
        "Stop",
        "StopFailure",
        "SessionStart",
        "SessionEnd",
        "Notification",
    ] {
        hooks.insert(ev.into(), plain.clone());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks }))?
    );
    Ok(())
}
