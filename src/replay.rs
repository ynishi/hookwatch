//! Replay: run jigs over the recorded PreToolUse calls, and say what then
//! happened to each call the jig would have hit. Pure but for the runner.

use crate::episode::Ev;
use crate::jig::{Engine, RunOutcome, Runner, ToolCall};
use std::collections::BTreeMap;

pub struct JigReplay {
    pub jig: String,
    pub hits: usize,
    /// outcome → count. Outcomes: ran, denied, failed, cut_in, no_post.
    pub outcomes: BTreeMap<&'static str, usize>,
    /// Up to 3 hits: "outcome | evidence".
    pub samples: Vec<String>,
    /// Every hit: (outcome, tool_name, tool_input, evidence). For gates that
    /// must tell confirmed hits from unconfirmed ones.
    pub all: Vec<(&'static str, String, serde_json::Value, String)>,
}

pub struct Report {
    pub calls: usize,
    /// Calls whose input could not be read as a tool call, with why.
    pub unreadable: Vec<String>,
    pub command_runs: usize,
    /// Runs where a command jig itself failed: "jig: why".
    pub command_errors: Vec<String>,
    pub jigs: Vec<JigReplay>,
}

/// What happened after the PreToolUse at `i`, until its turn stopped.
/// - ran: its PostToolUse came
/// - denied / failed: PermissionDenied / PostToolUseFailure for the same call
/// - cut_in: the human wrote before the call finished or the turn stopped
/// - no_post: the turn stopped with no result (a hook blocked it, or it was cut off)
pub fn outcome(events: &[Ev], i: usize) -> &'static str {
    let call = &events[i];
    let input = call.payload.get("tool_input");
    let same = |e: &Ev| e.tool == call.tool && e.payload.get("tool_input") == input;
    for e in &events[i + 1..] {
        if e.session != call.session {
            break;
        }
        match e.kind.as_str() {
            "PostToolUse" if same(e) => return "ran",
            "PermissionDenied" if same(e) => return "denied",
            "PostToolUseFailure" if same(e) => return "failed",
            "UserPromptSubmit" => return "cut_in",
            "Stop" | "StopFailure" => break,
            _ => {}
        }
    }
    "no_post"
}

/// `events` ordered by session then time. `run` starts command jigs: replay
/// really runs them, on the recorded payloads.
pub fn run(engine: &Engine, events: &[Ev], run: &mut Runner) -> Report {
    let mut report = Report {
        calls: 0,
        unreadable: vec![],
        command_runs: 0,
        command_errors: vec![],
        jigs: vec![],
    };
    let mut by_jig: BTreeMap<String, JigReplay> = BTreeMap::new();
    for (i, e) in events.iter().enumerate() {
        crate::jig::EVENT_TS_MS.store(e.ts_ms, std::sync::atomic::Ordering::Relaxed);
        let hook_event = e
            .payload
            .get("hook_event_name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if e.kind != "PreToolUse" {
            // Other events reach only command jigs that name them; nothing
            // was stopped there, so a hit's outcome is just "event".
            if hook_event.is_empty() || hook_event == "PreToolUse" {
                continue;
            }
            let call =
                ToolCall::for_event(hook_event, e.tool.as_deref(), e.payload.get("tool_input"));
            let evaluation = engine.evaluate_with(&call, &e.payload.to_string(), run);
            for r in evaluation.runs {
                report.command_runs += 1;
                if let RunOutcome::Error(m) = r.outcome {
                    report.command_errors.push(format!("{}: {m}", r.jig_id));
                }
            }
            for h in evaluation.hits {
                let r = by_jig.entry(h.jig_id.clone()).or_insert_with(|| JigReplay {
                    jig: h.jig_id.clone(),
                    hits: 0,
                    outcomes: BTreeMap::new(),
                    samples: vec![],
                    all: vec![],
                });
                r.hits += 1;
                *r.outcomes.entry("event").or_insert(0) += 1;
                if r.samples.len() < 3 {
                    r.samples.push(format!("event | {}", h.evidence));
                }
                r.all.push((
                    "event",
                    hook_event.to_owned(),
                    serde_json::Value::Null,
                    h.evidence.clone(),
                ));
            }
            continue;
        }
        report.calls += 1;
        let (Some(tool), Some(input)) = (e.tool.as_deref(), e.payload.get("tool_input")) else {
            report.unreadable.push(format!(
                "event at {} has no tool_name or tool_input",
                e.ts_ms
            ));
            continue;
        };
        // The file on disk at that time is not recorded: Write is judged as a new file.
        let call = match ToolCall::from_claude(tool, input, None) {
            Ok(c) => c,
            Err(err) => {
                report
                    .unreadable
                    .push(format!("{tool} at {}: {err}", e.ts_ms));
                continue;
            }
        };
        // Command jigs get the payload as it was recorded.
        let payload = e.payload.to_string();
        let evaluation = engine.evaluate_with(&call, &payload, run);
        for r in evaluation.runs {
            report.command_runs += 1;
            if let RunOutcome::Error(m) = r.outcome {
                report.command_errors.push(format!("{}: {m}", r.jig_id));
            }
        }
        let hits = evaluation.hits;
        if hits.is_empty() {
            continue;
        }
        let out = outcome(events, i);
        for h in hits {
            let r = by_jig.entry(h.jig_id.clone()).or_insert_with(|| JigReplay {
                jig: h.jig_id.clone(),
                hits: 0,
                outcomes: BTreeMap::new(),
                samples: vec![],
                all: vec![],
            });
            r.hits += 1;
            *r.outcomes.entry(out).or_insert(0) += 1;
            if r.samples.len() < 3 {
                r.samples.push(format!("{out} | {}", h.evidence));
            }
            r.all
                .push((out, tool.to_owned(), input.clone(), h.evidence.clone()));
        }
    }
    report.jigs = by_jig.into_values().collect();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jig::{Action, Jig, Lifecycle, Rule};
    use serde_json::{Value, json};

    fn ev(ts: i64, kind: &str, tool: Option<&str>, payload: Value) -> Ev {
        Ev {
            session: "s".into(),
            ts_ms: ts,
            kind: kind.into(),
            tool: tool.map(Into::into),
            payload,
        }
    }

    fn bash(cmd: &str) -> Value {
        json!({"tool_input": {"command": cmd}})
    }

    #[test]
    fn outcome_follows_the_same_call() {
        let events = vec![
            ev(1, "PreToolUse", Some("Bash"), bash("ls")),
            ev(2, "PreToolUse", Some("Bash"), bash("rm x")),
            ev(3, "PostToolUse", Some("Bash"), bash("ls")),
            ev(4, "PermissionDenied", Some("Bash"), bash("rm x")),
            ev(
                5,
                "PreToolUse",
                Some("Bash"),
                bash("git commit --no-verify"),
            ),
            ev(6, "Stop", None, json!({})),
            ev(7, "UserPromptSubmit", None, json!({"prompt": "p"})),
            ev(8, "PreToolUse", Some("Bash"), bash("sleep 100")),
            ev(9, "UserPromptSubmit", None, json!({"prompt": "stop that"})),
        ];
        let got: Vec<_> = [0, 1, 4, 7].iter().map(|i| outcome(&events, *i)).collect();
        assert_eq!(got, vec!["ran", "denied", "no_post", "cut_in"]);
    }

    #[test]
    fn candidate_jig_report_counts_outcomes() {
        let jig = Jig {
            id: "no-rm".into(),
            description: String::new(),
            rule: Rule::BashRegex {
                pattern: r"^rm\s".into(),
            },
            action: Action::Block,
            lifecycle: Lifecycle::Shadow,
            message: String::new(),
            when: Default::default(),
            repeat: Default::default(),
        };
        let engine = Engine::new(vec![jig]).unwrap();
        let events = vec![
            ev(1, "PreToolUse", Some("Bash"), bash("rm a")),
            ev(2, "PostToolUse", Some("Bash"), bash("rm a")),
            ev(3, "PreToolUse", Some("Bash"), bash("rm b")),
            ev(4, "PermissionDenied", Some("Bash"), bash("rm b")),
            ev(5, "PreToolUse", Some("Bash"), json!({})),
        ];
        let r = run(&engine, &events, &mut |_, _, _| {
            anyhow::bail!("no command jigs here")
        });
        assert_eq!(r.calls, 3);
        assert_eq!(r.unreadable.len(), 1);
        assert_eq!(r.jigs[0].hits, 2);
        assert_eq!(r.jigs[0].outcomes.get("ran"), Some(&1));
        assert_eq!(r.jigs[0].outcomes.get("denied"), Some(&1));
    }
}
