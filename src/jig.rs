//! Jig (治具): a trigger, a deterministic predicate (L1), what to do on a hit,
//! and where the jig is in its lifecycle. Evaluation is pure: the caller
//! hands in the tool call and, for Write, the file content that is on disk now.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    /// Record what it would have done; never touch the agent.
    Shadow,
    /// Act on the agent.
    Enforce,
    /// Not evaluated.
    Retired,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Shadow => "shadow",
            Lifecycle::Enforce => "enforce",
            Lifecycle::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Exit 2 on PreToolUse: the tool call does not run.
    Block,
    /// Let it run, and tell the agent why it looks wrong.
    Nudge,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Block => "block",
            Action::Nudge => "nudge",
        }
    }
}

/// L1 predicates. Every one is deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// Regex on the Bash command.
    BashRegex { pattern: String },
    /// Regex on the text a Write / Edit / MultiEdit puts into a file.
    ContentRegex { pattern: String },
    /// Protects paths from writes: regex on the target path of a file tool
    /// that writes (Edit / Write / MultiEdit / NotebookEdit; not Read), and on
    /// a Bash command that both names the path and looks like a write
    /// (`bash_writes`). Reading a protected path is not a hit.
    PathRegex { pattern: String },
    /// Any program (a Python script, a shell script, …) that hookwatch runs
    /// with the hook payload on stdin. Exit 0 = no hit, exit 2 = hit (stderr
    /// is the evidence), anything else or no exit within `timeout_ms` = the
    /// jig failed. hookwatch wraps it: it times every run, and in Shadow a
    /// hit is only recorded, never acted on.
    Command {
        argv: Vec<String>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
}

fn default_timeout_ms() -> u64 {
    2000
}

/// A command jig's argv may start an element with `$HOME/`, so a shared
/// jigs.json need not carry one machine's home directory.
pub fn expand_home(arg: &str, home: &str) -> String {
    match arg.strip_prefix("$HOME/") {
        Some(rest) => format!("{}/{rest}", home.trim_end_matches('/')),
        None => arg.to_owned(),
    }
}

/// What running a command jig gave.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// None: killed at the timeout.
    pub exit: Option<i32>,
    pub stderr: String,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Pass,
    Hit,
    /// The jig itself failed (bad exit, timeout, could not start).
    Error(String),
}

impl RunOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunOutcome::Pass => "pass",
            RunOutcome::Hit => "hit",
            RunOutcome::Error(_) => "error",
        }
    }
}

/// One run of a command jig, for the stopwatch.
#[derive(Debug, Clone)]
pub struct Run {
    pub jig_id: String,
    pub ms: u64,
    pub exit: Option<i32>,
    pub outcome: RunOutcome,
}

pub struct Evaluation {
    pub hits: Vec<Hit>,
    pub runs: Vec<Run>,
}

/// Starts a command jig: (argv, payload on stdin, timeout ms).
pub type Runner<'a> = dyn FnMut(&[String], &str, u64) -> anyhow::Result<RunResult> + 'a;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jig {
    pub id: String,
    pub description: String,
    pub rule: Rule,
    pub action: Action,
    pub lifecycle: Lifecycle,
    /// What the agent is told when the jig fires.
    pub message: String,
    /// Which calls the jig looks at. Checked before the rule, so a command
    /// jig is not even started for other calls (a Python start costs ~0.2 s,
    /// measured 2026-11-14).
    #[serde(default, skip_serializing_if = "When::is_any")]
    pub when: When,
    /// `once`: a hit whose evidence this jig already gave is not said again
    /// (the run is still timed). For jigs that report a standing state, such
    /// as the script bundles worth a tool, at every turn's end.
    #[serde(default, skip_serializing_if = "Repeat::is_always")]
    pub repeat: Repeat,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Always,
    Once,
}

impl Repeat {
    pub fn is_always(&self) -> bool {
        *self == Repeat::Always
    }
}

/// The time of the event a command jig is judging: now in the hook, the
/// recorded time in a replay. Handed to the script as HOOKWATCH_EVENT_TS_MS,
/// so a jig that reads the ledger sees it as it was then.
pub static EVENT_TS_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Empty fields match everything, but for `events` (empty = PreToolUse).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct When {
    /// Hook events, e.g. ["Stop"]. Empty: ["PreToolUse"]. Only command jigs
    /// may name others, and only to nudge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// Tool names, e.g. ["Write", "Edit"].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Regex on the target file path; a call without a path does not match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl When {
    pub fn is_any(&self) -> bool {
        self.events.is_empty() && self.tools.is_empty() && self.path.is_none()
    }

    pub fn events(&self) -> Vec<String> {
        if self.events.is_empty() {
            vec!["PreToolUse".into()]
        } else {
            self.events.clone()
        }
    }
}

/// The parts of a PreToolUse call the rules read.
#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    /// The hook event; "" reads as PreToolUse (what from_claude builds).
    pub hook_event: String,
    /// "" for an event without a tool (Stop, UserPromptSubmit, …).
    pub tool: String,
    pub command: Option<String>,
    pub path: Option<String>,
    /// (old, new) text pairs. Write has old = file on disk now (or "").
    pub edits: Vec<(String, String)>,
}

impl ToolCall {
    pub fn event(&self) -> &str {
        if self.hook_event.is_empty() {
            "PreToolUse"
        } else {
            &self.hook_event
        }
    }

    /// Any hook event, for command jigs: the tool fields when it has them
    /// (PostToolUse), none otherwise. Edits are not read.
    pub fn for_event(event: &str, tool: Option<&str>, input: Option<&serde_json::Value>) -> Self {
        let s = |k: &str| {
            input
                .and_then(|v| v.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        ToolCall {
            hook_event: event.to_owned(),
            tool: tool.unwrap_or("").to_owned(),
            command: s("command"),
            path: s("file_path").or_else(|| s("notebook_path")),
            edits: vec![],
        }
    }

    /// Build from Claude Code's `tool_name` / `tool_input`. `on_disk` is the
    /// current content of the target file for Write: `None` means the file
    /// does not exist yet (the caller tells that apart from a read error).
    /// A file tool without the fields it must have is an error, not "no edit".
    pub fn from_claude(
        tool: &str,
        input: &serde_json::Value,
        on_disk: Option<String>,
    ) -> anyhow::Result<Self> {
        fn opt(v: &serde_json::Value, k: &str) -> Option<String> {
            v.get(k).and_then(|v| v.as_str()).map(str::to_owned)
        }
        fn req(v: &serde_json::Value, tool: &str, k: &str) -> anyhow::Result<String> {
            opt(v, k).ok_or_else(|| anyhow::anyhow!("{tool} tool_input has no string `{k}`"))
        }
        let mut call = ToolCall {
            hook_event: String::new(),
            tool: tool.to_owned(),
            command: opt(input, "command"),
            path: opt(input, "file_path").or_else(|| opt(input, "notebook_path")),
            edits: vec![],
        };
        match tool {
            "Write" => {
                let before = match on_disk {
                    Some(s) => s,
                    None => String::new(), // a new file: nothing was there
                };
                call.edits.push((before, req(input, tool, "content")?));
            }
            "Edit" => call.edits.push((
                req(input, tool, "old_string")?,
                req(input, tool, "new_string")?,
            )),
            "MultiEdit" => {
                let arr = input
                    .get("edits")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| anyhow::anyhow!("MultiEdit tool_input has no `edits` array"))?;
                for e in arr {
                    call.edits
                        .push((req(e, tool, "old_string")?, req(e, tool, "new_string")?));
                }
            }
            _ => {}
        }
        Ok(call)
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub jig_id: String,
    pub action: Action,
    pub lifecycle: Lifecycle,
    pub message: String,
    /// What the rule saw, for `hookwatch why`.
    pub evidence: String,
}

/// Compiled rules, so the regexes are built once per process.
pub struct Engine {
    jigs: Vec<(Jig, Compiled, CompiledWhen)>,
}

struct CompiledWhen {
    events: Vec<String>,
    tools: Vec<String>,
    path: Option<Regex>,
}

impl CompiledWhen {
    fn matches(&self, call: &ToolCall) -> bool {
        if !self.events.iter().any(|e| *e == call.event()) {
            return false;
        }
        let tool_ok = self.tools.is_empty() || self.tools.iter().any(|t| *t == call.tool);
        let path_ok = match (&self.path, &call.path) {
            (None, _) => true,
            (Some(re), Some(p)) => re.is_match(p),
            (Some(_), None) => false,
        };
        tool_ok && path_ok
    }
}

enum Compiled {
    Regex(Regex),
    Command,
}

impl Engine {
    pub fn new(jigs: Vec<Jig>) -> anyhow::Result<Self> {
        let mut out = vec![];
        for j in jigs {
            let c = match &j.rule {
                Rule::BashRegex { pattern }
                | Rule::ContentRegex { pattern }
                | Rule::PathRegex { pattern } => Compiled::Regex(
                    Regex::new(pattern)
                        .map_err(|e| anyhow::anyhow!("jig {}: bad pattern: {e}", j.id))?,
                ),
                Rule::Command { argv, .. } => {
                    if argv.is_empty() {
                        anyhow::bail!("jig {}: command argv is empty", j.id);
                    }
                    Compiled::Command
                }
            };
            let events = j.when.events();
            if events.iter().any(|e| e != "PreToolUse") {
                // Regex rules read a tool call; only a script can judge a
                // turn's end or a result. And only a call not yet made can
                // be stopped.
                if !matches!(j.rule, Rule::Command { .. }) {
                    anyhow::bail!(
                        "jig {}: only a command jig can watch events other than PreToolUse",
                        j.id
                    );
                }
                if j.action == Action::Block {
                    anyhow::bail!(
                        "jig {}: a block can only act on PreToolUse; use nudge",
                        j.id
                    );
                }
            }
            let w = CompiledWhen {
                events,
                tools: j.when.tools.clone(),
                path: match &j.when.path {
                    Some(p) => Some(
                        Regex::new(p)
                            .map_err(|e| anyhow::anyhow!("jig {}: bad when.path: {e}", j.id))?,
                    ),
                    None => None,
                },
            };
            out.push((j, c, w));
        }
        Ok(Engine { jigs: out })
    }

    pub fn jigs_all(&self) -> impl Iterator<Item = &Jig> {
        self.jigs.iter().map(|(j, _, _)| j)
    }

    #[cfg(test)]
    pub fn jigs(&self) -> impl Iterator<Item = &Jig> {
        self.jigs.iter().map(|(j, _, _)| j)
    }

    /// Only the deterministic rules; command jigs count as not run. For tests.
    #[cfg(test)]
    pub fn evaluate(&self, call: &ToolCall) -> Vec<Hit> {
        self.evaluate_with(call, "{}", &mut |_, _, _| {
            anyhow::bail!("not run in this test")
        })
        .hits
    }

    /// `payload` is the raw hook input, handed to command jigs on stdin.
    pub fn evaluate_with(&self, call: &ToolCall, payload: &str, run: &mut Runner) -> Evaluation {
        let mut hits = vec![];
        let mut runs = vec![];
        for (jig, c, when) in &self.jigs {
            if jig.lifecycle == Lifecycle::Retired || !when.matches(call) {
                continue;
            }
            let evidence = match (&jig.rule, c) {
                (Rule::Command { argv, timeout_ms }, Compiled::Command) => {
                    let (r, evidence) = run_command(&jig.id, argv, payload, *timeout_ms, run);
                    runs.push(r);
                    evidence
                }
                (Rule::BashRegex { .. }, Compiled::Regex(re)) => {
                    call.command.as_deref().and_then(|cmd| {
                        re.find(cmd)
                            .map(|m| format!("command matched `{}`", m.as_str()))
                    })
                }
                (Rule::PathRegex { .. }, Compiled::Regex(re)) => {
                    let p = if WRITE_TOOLS.contains(&call.tool.as_str()) {
                        call.path.as_deref().and_then(|p| {
                            re.find(p).map(|m| format!("path matched `{}`", m.as_str()))
                        })
                    } else {
                        None
                    };
                    p.or_else(|| {
                        call.command.as_deref().and_then(|cmd| {
                            let m = re.find(cmd)?;
                            let verb = bash_writes(cmd)?;
                            Some(format!(
                                "command writes ({verb}) and names `{}`",
                                m.as_str()
                            ))
                        })
                    })
                }
                (Rule::ContentRegex { .. }, Compiled::Regex(re)) => {
                    call.edits.iter().find_map(|(old, new)| {
                        // Only text this call adds counts, not what was there.
                        let added = re.find_iter(new).count() > re.find_iter(old).count();
                        if added {
                            re.find(new).map(|m| {
                                let shown: String = m.as_str().chars().take(12).collect();
                                format!("new content matched `{shown}…`")
                            })
                        } else {
                            None
                        }
                    })
                }
                _ => unreachable!("rule and compiled form are built together"),
            };
            if let Some(evidence) = evidence {
                let at = match &call.path {
                    Some(p) => format!("{} {p}", call.tool),
                    None if call.tool.is_empty() => call.event().to_owned(),
                    None => call.tool.clone(),
                };
                hits.push(Hit {
                    jig_id: jig.id.clone(),
                    action: jig.action,
                    lifecycle: jig.lifecycle,
                    message: jig.message.clone(),
                    evidence: format!("{at}: {evidence}"),
                });
            }
        }
        Evaluation { hits, runs }
    }
}

/// Run one command jig and read its exit: (the run, the evidence on a hit).
fn run_command(
    jig_id: &str,
    argv: &[String],
    payload: &str,
    timeout_ms: u64,
    run: &mut Runner,
) -> (Run, Option<String>) {
    let (ms, exit, outcome, evidence) = match run(argv, payload, timeout_ms) {
        Err(e) => (
            0,
            None,
            RunOutcome::Error(format!("could not run: {e:#}")),
            None,
        ),
        Ok(r) => {
            let said: String = r.stderr.trim().chars().take(300).collect();
            match r.exit {
                Some(0) => (r.ms, r.exit, RunOutcome::Pass, None),
                Some(2) => {
                    let ev = if said.is_empty() {
                        "script exited 2".to_owned()
                    } else {
                        said
                    };
                    (r.ms, r.exit, RunOutcome::Hit, Some(ev))
                }
                Some(n) => (
                    r.ms,
                    r.exit,
                    RunOutcome::Error(format!("exit {n}: {said}")),
                    None,
                ),
                None => (
                    r.ms,
                    None,
                    RunOutcome::Error(format!("no exit within {timeout_ms} ms")),
                    None,
                ),
            }
        }
    };
    (
        Run {
            jig_id: jig_id.to_owned(),
            ms,
            exit,
            outcome,
        },
        evidence,
    )
}

const WRITE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Whether a shell command looks like it writes, and the first sign of it.
/// Heuristic on purpose: an L1 rule that is cheap and explainable. It errs
/// toward "writes" (`cat a > b` is a write even if `b` is not protected).
pub fn bash_writes(cmd: &str) -> Option<String> {
    use std::sync::LazyLock;
    // Redirections that write nothing: fd duplication and /dev/null.
    static HARMLESS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\d*>&\d+|&?\d*>>?\s*/dev/null\b").unwrap());
    static VERB: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            // A verb counts only in command position, so `grep rm f` is not a write.
            r"(^\s*|[;&|(]\s*|\b(sudo|xargs|exec)\s+)(rm|rmdir|mv|cp|tee|truncate|chmod|chown|ln|touch|install|dd|unlink|shred)\s|\b(sed|perl)\s+(-\w+\s+)*?-\w*i\b|>>?",
        )
        .unwrap()
    });
    let cleaned = HARMLESS.replace_all(cmd, " ");
    if let Some(m) = VERB.find(&cleaned) {
        let sign = m.as_str().trim_start_matches([';', '&', '|', '(']).trim();
        return Some(sign.to_owned());
    }
    // sqlite3 writes unless opened read-only.
    if cleaned.contains("sqlite3") && !cleaned.contains("-readonly") {
        return Some("sqlite3 without -readonly".into());
    }
    None
}

/// The jigs hookwatch ships with.
pub fn builtin() -> Vec<Jig> {
    let j = |id: &str, description: &str, rule: Rule, action, lifecycle, message: &str| Jig {
        id: id.into(),
        description: description.into(),
        rule,
        action,
        lifecycle,
        message: message.into(),
        when: When::default(),
        repeat: Repeat::Always,
    };
    vec![
        j(
            "no-verify",
            "git commit / push that skips the hooks",
            Rule::BashRegex {
                pattern: r"\bgit\b[^|;&]*\s(--no-verify|-n\b)".into(),
            },
            Action::Block,
            Lifecycle::Enforce,
            "Skipping git hooks (--no-verify) is not allowed. Fix what the hook reports instead.",
        ),
        j(
            "secret-in-file",
            "private key or cloud credential written into a file",
            Rule::ContentRegex {
                pattern: r"-----BEGIN [A-Z ]*PRIVATE KEY-----|\bAKIA[0-9A-Z]{16}\b|\bgh[pousr]_[A-Za-z0-9]{36,}\b|\bsk-ant-[A-Za-z0-9_-]{20,}".into(),
            },
            Action::Block,
            Lifecycle::Enforce,
            "This writes a secret into a file. Read it from the environment or a secret store instead.",
        ),
        j(
            "tamper-guard",
            "changes to hook settings or to hookwatch's own state",
            Rule::PathRegex {
                pattern: r"\.claude/settings(\.local)?\.json|\.hookwatch/".into(),
            },
            Action::Block,
            Lifecycle::Enforce,
            "Hook settings and hookwatch's state are changed by the human, not by the agent.",
        ),
    ]
}

/// User file: extra jigs and lifecycle overrides by jig id.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct JigFile {
    #[serde(default)]
    pub jigs: Vec<Jig>,
    #[serde(default)]
    pub lifecycle: BTreeMap<String, Lifecycle>,
}

pub fn merge(mut base: Vec<Jig>, file: JigFile) -> Vec<Jig> {
    base.extend(file.jigs);
    for j in &mut base {
        if let Some(l) = file.lifecycle.get(&j.id) {
            j.lifecycle = *l;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine() -> Engine {
        Engine::new(builtin()).unwrap()
    }

    fn ids(h: &[Hit]) -> Vec<&str> {
        h.iter().map(|h| h.jig_id.as_str()).collect()
    }

    #[test]
    fn no_verify_hits_commit_and_leaves_plain_commit() {
        let e = engine();
        let c = ToolCall::from_claude(
            "Bash",
            &json!({"command": "git commit --no-verify -m x"}),
            None,
        )
        .unwrap();
        assert_eq!(ids(&e.evaluate(&c)), vec!["no-verify"]);
        let c = ToolCall::from_claude(
            "Bash",
            &json!({"command": "git commit -m 'no verify needed'"}),
            None,
        )
        .unwrap();
        assert!(e.evaluate(&c).is_empty());
    }

    // test-weakening moved to jigs/tests_weakened.py (its tests are there).

    #[test]
    fn event_scoped_command_jig_runs_only_on_its_event() {
        let mut j = cmd_jig("stop");
        j.action = Action::Nudge;
        j.when.events = vec!["Stop".into()];
        let e = Engine::new(vec![j]).unwrap();
        let mut started = 0;
        let mut run = |_: &[String], _: &str, _: u64| {
            started += 1;
            Ok(RunResult {
                exit: Some(2),
                stderr: "candidates: a".into(),
                ms: 1,
            })
        };
        assert!(e.evaluate_with(&bash("ls"), "{}", &mut run).hits.is_empty());
        let stop = ToolCall::for_event("Stop", None, None);
        let ev = e.evaluate_with(&stop, "{}", &mut run);
        assert_eq!(ids(&ev.hits), vec!["stop"]);
        assert_eq!(ev.hits[0].evidence, "Stop: candidates: a");
        assert_eq!(started, 1);
    }

    #[test]
    fn other_events_are_for_command_nudges_only() {
        let mut block = cmd_jig("b");
        block.when.events = vec!["Stop".into()];
        assert!(Engine::new(vec![block]).is_err()); // cmd_jig blocks
        let f: JigFile = serde_json::from_value(json!({"jigs": [{
            "id": "r", "description": "", "rule": {"kind": "bash_regex", "pattern": "x"},
            "action": "nudge", "lifecycle": "shadow", "message": "m", "when": {"events": ["PostToolUse"]}}]}))
        .unwrap();
        assert!(Engine::new(f.jigs).is_err());
    }

    #[test]
    fn secret_only_when_this_call_adds_it() {
        let e = engine();
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let c = ToolCall::from_claude("Write", &json!({"file_path": "/r/k", "content": key}), None)
            .unwrap();
        assert_eq!(ids(&e.evaluate(&c)), vec!["secret-in-file"]);
        let c = ToolCall::from_claude(
            "Edit",
            &json!({"file_path": "/r/k", "old_string": format!("{key}\nA"), "new_string": format!("{key}\nB")}),
            None,
        )
        .unwrap();
        assert!(e.evaluate(&c).is_empty());
    }

    #[test]
    fn tamper_guard_on_path_and_command() {
        let e = engine();
        let c = ToolCall::from_claude(
            "Edit",
            &json!({"file_path": "/r/.claude/settings.json", "old_string": "a", "new_string": "b"}),
            None,
        )
        .unwrap();
        assert_eq!(ids(&e.evaluate(&c)), vec!["tamper-guard"]);
        let c = ToolCall::from_claude(
            "Bash",
            &json!({"command": "rm $HOME/.hookwatch/ledger.db"}),
            None,
        )
        .unwrap();
        assert_eq!(ids(&e.evaluate(&c)), vec!["tamper-guard"]);
    }

    fn bash(cmd: &str) -> ToolCall {
        ToolCall::from_claude("Bash", &json!({ "command": cmd }), None).unwrap()
    }

    /// Reads of protected paths: the false positives seen on 2026-11-14
    /// (verdict 77beb2c8, `sqlite3 -readonly $HOME/.hookwatch/ledger.db ...`).
    #[test]
    fn tamper_guard_lets_reads_through() {
        let e = engine();
        for cmd in [
            "sqlite3 -readonly $HOME/.hookwatch/ledger.db \"SELECT 1\"",
            "ls -la $HOME/.hookwatch 2>&1",
            "cat .claude/settings.local.json >/dev/null",
            "jq . .claude/settings.json 2>&1 | head",
        ] {
            assert!(e.evaluate(&bash(cmd)).is_empty(), "{cmd}");
        }
        let read = ToolCall::from_claude(
            "Read",
            &json!({"file_path": "/r/.claude/settings.json"}),
            None,
        )
        .unwrap();
        assert!(e.evaluate(&read).is_empty());
    }

    #[test]
    fn tamper_guard_still_stops_writes_by_bash() {
        let e = engine();
        for cmd in [
            "sqlite3 $HOME/.hookwatch/ledger.db 'DELETE FROM verdicts'",
            "sed -i '' 's/a/b/' .claude/settings.local.json",
            "jq '.hooks={}' x.json > .claude/settings.local.json",
            "cd /r && mv tmp.json .claude/settings.json",
            "echo '{}' | tee $HOME/.hookwatch/jigs.json",
        ] {
            assert_eq!(ids(&e.evaluate(&bash(cmd))), vec!["tamper-guard"], "{cmd}");
        }
    }

    #[test]
    fn bash_writes_names_the_sign() {
        assert_eq!(bash_writes("rm -rf x").as_deref(), Some("rm"));
        assert_eq!(bash_writes("a && mv b c").as_deref(), Some("mv"));
        assert_eq!(
            bash_writes("perl -pi -e s/a/b/ f").as_deref(),
            Some("perl -pi")
        );
        assert_eq!(bash_writes("ls 2>&1 >/dev/null"), None);
        assert_eq!(bash_writes("grep rm file"), None);
    }

    #[test]
    fn expand_home_only_at_the_start() {
        assert_eq!(expand_home("$HOME/j/a.py", "/h/u/"), "/h/u/j/a.py");
        assert_eq!(expand_home("python3", "/h/u"), "python3");
        assert_eq!(expand_home("x$HOME/a", "/h/u"), "x$HOME/a");
    }

    fn cmd_jig(id: &str) -> Jig {
        Jig {
            id: id.into(),
            description: String::new(),
            rule: Rule::Command {
                argv: vec!["python3".into(), format!("{id}.py")],
                timeout_ms: 100,
            },
            action: Action::Block,
            lifecycle: Lifecycle::Shadow,
            message: "m".into(),
            when: When::default(),
            repeat: Repeat::Always,
        }
    }

    /// The 2026-11-14 measure: term-shadow-md ran Python on every Bash call.
    #[test]
    fn when_skips_calls_before_the_rule_runs() {
        let mut j = cmd_jig("md-only");
        j.when = When {
            events: vec![],
            tools: vec!["Write".into(), "Edit".into()],
            path: Some(r"\.md$".into()),
        };
        let e = Engine::new(vec![j]).unwrap();
        let mut started = 0;
        let mut run = |_: &[String], _: &str, _: u64| {
            started += 1;
            Ok(RunResult {
                exit: Some(0),
                stderr: String::new(),
                ms: 1,
            })
        };
        let write = |p: &str| {
            ToolCall::from_claude("Write", &json!({"file_path": p, "content": ""}), None).unwrap()
        };
        assert_eq!(e.evaluate_with(&bash("ls"), "{}", &mut run).runs.len(), 0);
        assert_eq!(
            e.evaluate_with(&write("/r/a.rs"), "{}", &mut run)
                .runs
                .len(),
            0
        );
        assert_eq!(
            e.evaluate_with(&write("/r/a.md"), "{}", &mut run)
                .runs
                .len(),
            1
        );
        assert_eq!(started, 1);
    }

    #[test]
    fn when_scopes_a_regex_jig_and_bad_when_regex_is_refused() {
        let f: JigFile = serde_json::from_value(json!({"jigs": [{
            "id": "md", "description": "", "rule": {"kind": "content_regex", "pattern": "\\bshadow\\b"},
            "action": "nudge", "lifecycle": "enforce", "message": "m", "when": {"path": "\\.md$"}}]}))
        .unwrap();
        let e = Engine::new(f.jigs).unwrap();
        let w = |p: &str| {
            ToolCall::from_claude("Write", &json!({"file_path": p, "content": "shadow"}), None)
                .unwrap()
        };
        assert_eq!(ids(&e.evaluate(&w("/r/a.md"))), vec!["md"]);
        assert!(e.evaluate(&w("/r/a.rs")).is_empty());
        let mut bad = cmd_jig("x");
        bad.when.path = Some("(".into());
        assert!(Engine::new(vec![bad]).is_err());
    }

    #[test]
    fn command_jig_exit_codes_map_to_hit_pass_and_error() {
        let e = Engine::new(vec![
            cmd_jig("pass"),
            cmd_jig("hit"),
            cmd_jig("crash"),
            cmd_jig("slow"),
            cmd_jig("missing"),
        ])
        .unwrap();
        let call = bash("git push");
        let mut seen_payload = String::new();
        let ev = e.evaluate_with(&call, r#"{"x":1}"#, &mut |argv, payload, timeout| {
            seen_payload = payload.to_owned();
            let r = |exit, stderr: &str| {
                Ok(RunResult {
                    exit,
                    stderr: stderr.into(),
                    ms: 7,
                })
            };
            match argv[1].as_str() {
                "pass.py" => r(Some(0), ""),
                "hit.py" => r(Some(2), "  pushes to main\n"),
                "crash.py" => r(Some(1), "Traceback"),
                "slow.py" => Ok(RunResult {
                    exit: None,
                    stderr: String::new(),
                    ms: timeout,
                }),
                _ => anyhow::bail!("no such file"),
            }
        });
        assert_eq!(seen_payload, r#"{"x":1}"#);
        assert_eq!(ids(&ev.hits), vec!["hit"]);
        assert_eq!(ev.hits[0].evidence, "Bash: pushes to main");
        let outcomes: Vec<_> = ev.runs.iter().map(|r| r.outcome.as_str()).collect();
        assert_eq!(outcomes, vec!["pass", "hit", "error", "error", "error"]);
        assert_eq!(
            ev.runs[3].outcome,
            RunOutcome::Error("no exit within 100 ms".into())
        );
        assert!(matches!(&ev.runs[4].outcome, RunOutcome::Error(m) if m.contains("no such file")));
    }

    #[test]
    fn command_jig_from_json_gets_default_timeout_and_empty_argv_is_refused() {
        let f: JigFile = serde_json::from_value(json!({"jigs": [{
            "id": "py", "description": "", "rule": {"kind": "command", "argv": ["python3", "x.py"]},
            "action": "block", "lifecycle": "shadow", "message": "m"}]}))
        .unwrap();
        assert!(matches!(
            f.jigs[0].rule,
            Rule::Command {
                timeout_ms: 2000,
                ..
            }
        ));
        let mut bad = cmd_jig("x");
        bad.rule = Rule::Command {
            argv: vec![],
            timeout_ms: 1,
        };
        assert!(Engine::new(vec![bad]).is_err());
    }

    #[test]
    fn file_overrides_lifecycle_and_adds_jigs() {
        let f: JigFile = serde_json::from_value(json!({
            "lifecycle": {"tamper-guard": "shadow", "no-verify": "retired"},
            "jigs": [{"id": "no-force-push", "description": "", "rule": {"kind": "bash_regex", "pattern": "push\\s+.*--force"},
                      "action": "nudge", "lifecycle": "enforce", "message": "m"}]
        }))
        .unwrap();
        let e = Engine::new(merge(builtin(), f)).unwrap();
        let c = ToolCall::from_claude(
            "Bash",
            &json!({"command": "git push --force --no-verify"}),
            None,
        )
        .unwrap();
        assert_eq!(ids(&e.evaluate(&c)), vec!["no-force-push"]);
        let tg = e.jigs().find(|j| j.id == "tamper-guard").unwrap();
        assert_eq!(tg.lifecycle, Lifecycle::Shadow);
    }
}
