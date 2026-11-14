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
    /// Label a verdict: the human says the jig was wrong or right.
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
    Stats,
    /// The recorded hook events, oldest first. The way to read the ledger:
    /// an agent does not need to open hookwatch's state files itself.
    Events {
        /// Only sessions whose id starts with this.
        #[arg(long)]
        session: Option<String>,
        /// Only these events (comma separated), e.g. UserPromptSubmit,Stop.
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
        /// How many of the newest events to show.
        #[arg(long, default_value_t = 50)]
        last: i64,
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
            let l = ledger()?;
            let v = l.verdict_by_short(&id)?;
            let label = if wrong { "wrong" } else { "right" };
            l.add_label(&new_id(), now_ms(), &v.id, label, note.as_deref())?;
            println!("labelled {} ({}) as {label}", short(&v.id), v.jig);
            Ok(())
        }
        Cmd::Stats => stats(),
        Cmd::Events {
            session,
            kinds,
            last,
        } => events(session, &kinds, last),
        Cmd::Episodes {
            json,
            last,
            context,
        } => episodes(json, last, context),
        Cmd::Replay { file } => replay(file),
        Cmd::SmithInput { last, repo } => smith_input(last, &repo),
        Cmd::Propose { file, dry_run } => propose(&file, dry_run),
        Cmd::Proposals { id } => proposals(id),
        Cmd::Accept { id } => accept(&id),
        Cmd::PutJigs { file } => put_jigs(&file),
        Cmd::Reject { id, why } => {
            need_terminal("reject")?;
            let l = ledger()?;
            let p = l.proposal_by_short(&id)?;
            l.decide_proposal(&p.id, "rejected", Some(&why), now_ms())?;
            println!("rejected {} ({})", short(&p.id), p.intent);
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
    if event != "PreToolUse" {
        return Ok(0);
    }
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
    let call = ToolCall::from_claude(tool, input, on_disk)?;
    let engine = Engine::new(load_jigs()?)?;
    let evaluation = engine.evaluate_with(&call, &raw, &mut spawn_jig);
    for r in &evaluation.runs {
        l.add_run(&new_id(), ts, &event_id, r)?;
    }
    let hits = evaluation.hits;

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
        eprintln!("{text}");
        return Ok(2);
    }
    if !nudges.is_empty() {
        let out = serde_json::json!({
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": nudges.join("\n")}
        });
        println!("{out}");
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

fn episodes(json: bool, last: usize, context: usize) -> anyhow::Result<()> {
    let l = ledger()?;
    let all = episode::episodes(&load_evs(&l)?, &l.rework_rows()?, &l.label_rows()?, context);
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
    out.sort();
    Ok(out)
}

fn smith_input(last: usize, repo: &std::path::Path) -> anyhow::Result<()> {
    let l = ledger()?;
    let evs = load_evs(&l)?;
    let all = episode::episodes(&evs, &l.rework_rows()?, &l.label_rows()?, 5);
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
    let parsed: JigFile =
        serde_json::from_str(&s).with_context(|| format!("parse {}", file.display()))?;
    // Refuse a file the hook could not load: bad regexes, empty argv.
    Engine::new(jig::merge(jig::builtin(), parsed))?;
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
    need_terminal("accept")?;
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
    l.decide_proposal(&p.id, "accepted", None, now_ms())?;
    println!("accepted {}", short(&p.id));
    Ok(())
}

/// Run a command jig as a child: payload on stdin, killed at the timeout.
/// stdout is not read by hookwatch (a jig speaks through exit and stderr).
fn spawn_jig(argv: &[String], payload: &str, timeout_ms: u64) -> anyhow::Result<jig::RunResult> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let home = std::env::var("HOME").context("HOME is not set; needed to expand $HOME/ in argv")?;
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

/// At Stop: keep the content of every file the agent edited in this turn.
fn take_snapshots(l: &Ledger, session: &str, ts: i64) -> anyhow::Result<()> {
    let mut paths = std::collections::BTreeSet::new();
    for p in l.post_tool_payloads_this_turn(session)? {
        let v: serde_json::Value =
            serde_json::from_str(&p).context("ledger payload is not JSON")?;
        if let Some(path) = rework::edited_path(&v)? {
            paths.insert(path);
        }
    }
    for path in paths {
        // A file the agent deleted has nothing to compare later.
        if let Some(content) = read_if_exists(&path)? {
            l.put_snapshot(&new_id(), session, &path, ts, &content)?;
        }
    }
    Ok(())
}

/// At the next prompt (or session end): what did the human change since?
fn check_rework(l: &Ledger, session: &str, ts: i64) -> anyhow::Result<()> {
    for snap in l.open_snapshots(session)? {
        let now = match read_if_exists(&snap.path)? {
            Some(s) => s,
            None => String::new(), // the human deleted it: every line removed
        };
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

fn stats() -> anyhow::Result<()> {
    let l = ledger()?;
    let stream: Vec<(String, i64, String)> = load_evs(&l)?
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
    let r = l.rework_totals()?;
    let stops = f.by_event.get("Stop").copied().unwrap_or(0); // no Stop event yet
    println!(
        "rework               {} of {} stopped turns, {} files, +{} -{} lines",
        r.turns, stops, r.files, r.added, r.removed
    );
    for (path, a, d, t) in l.recent_rework(5)? {
        println!("  {:>4} ago  +{a} -{d}  {path}", ago(t));
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
