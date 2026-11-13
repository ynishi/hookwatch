//! Episodes: each friction (a cut-in, a denial, a failed tool, a rework, a
//! label on a verdict) bundled with what led to it: the human's goal for the
//! turn and the agent's last tool calls. The material an AI reads to write a
//! jig. Pure: the caller loads the ledger.

use crate::ledger::{LabelRow, ReworkRow};
use serde::Serialize;
use serde_json::{Value, json};

/// One recorded event, with its kind from `friction::classify`.
pub struct Ev {
    pub session: String,
    pub ts_ms: i64,
    pub kind: String,
    pub tool: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Serialize)]
pub struct Episode {
    pub kind: &'static str,
    pub session: String,
    pub ts_ms: i64,
    /// The human's last prompt before the friction: what the agent was asked.
    pub goal: Option<String>,
    /// The agent's last tool calls before the friction, oldest first.
    pub recent: Vec<String>,
    pub detail: Value,
    /// Where in the session's conversation (Claude Code's JSONL) to read the
    /// turn that led here. hookwatch keeps the pointer, not the conversation.
    pub transcript: Option<TranscriptWindow>,
}

/// A time window in a transcript JSONL. Its lines carry `timestamp` in the
/// same ISO form, so `from <= timestamp <= to` selects the turn.
#[derive(Debug, Serialize, PartialEq)]
pub struct TranscriptWindow {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// ms since the Unix epoch → `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC), the form of
/// the transcript's `timestamp`.
pub fn iso_utc(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s, milli) = (
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
    );
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// How much before the friction to read when no prompt opened the turn.
const NO_PROMPT_WINDOW_MS: i64 = 10 * 60 * 1000;
/// The transcript stamps a prompt a little before the hook sees it: a window
/// starting at the hook's time missed the prompt itself (2026-11-14).
const WINDOW_SLACK_MS: i64 = 5000;
/// Errors and outputs longer than this are cut in episodes.
const DETAIL_MAX_CHARS: usize = 1500;

fn cut(v: Option<&Value>) -> Value {
    match v.and_then(|v| v.as_str()) {
        Some(s) if s.chars().count() > DETAIL_MAX_CHARS => {
            let head: String = s.chars().take(DETAIL_MAX_CHARS).collect();
            Value::String(format!("{head}… (cut; full text in the transcript)"))
        }
        _ => v.cloned().unwrap_or(Value::Null), // absent stays absent
    }
}

/// One line about an event: what it was about, cut to `max` characters.
pub fn summary(kind: &str, tool: Option<&str>, payload: &Value, max: usize) -> String {
    let s = |p: &str| payload.pointer(p).and_then(|v| v.as_str());
    let text = match kind {
        "UserPromptSubmit" | "AgentMessage" => s("/prompt").map(|p| format!("prompt: {p}")),
        "Notification" => s("/message").map(str::to_owned),
        _ => s("/tool_input/command")
            .or_else(|| s("/tool_input/file_path"))
            .or_else(|| s("/tool_input/description"))
            .or_else(|| s("/tool_input/pattern"))
            .map(str::to_owned),
    };
    let body: String = match text {
        Some(t) => t.replace('\n', " ").chars().take(max).collect(),
        None => String::new(), // an event with nothing to show (Stop, SessionEnd)
    };
    match tool {
        Some(t) => format!("{t}: {body}"),
        None => body,
    }
}

fn prompt(e: &Ev) -> Option<String> {
    e.payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// `events` ordered by session then time.
pub fn episodes(
    events: &[Ev],
    reworks: &[ReworkRow],
    labels: &[LabelRow],
    context: usize,
) -> Vec<Episode> {
    // Context of (session, ts): the last human prompt strictly before ts,
    // and the last `context` PreToolUse calls strictly before ts.
    let ctx = |session: &str, ts: i64| -> (Option<String>, Vec<String>, Option<TranscriptWindow>) {
        let before: Vec<&Ev> = events
            .iter()
            .filter(|e| e.session == session && e.ts_ms < ts)
            .collect();
        let goal_ev = before.iter().rev().find(|e| e.kind == "UserPromptSubmit");
        let goal = goal_ev.and_then(|e| prompt(e));
        let transcript = events
            .iter()
            .filter(|e| e.session == session)
            .find_map(|e| e.payload.get("transcript_path").and_then(|v| v.as_str()))
            .map(|path| TranscriptWindow {
                path: path.to_owned(),
                from: iso_utc(
                    match goal_ev {
                        Some(g) => g.ts_ms,
                        None => ts - NO_PROMPT_WINDOW_MS,
                    } - WINDOW_SLACK_MS,
                ),
                to: iso_utc(ts + WINDOW_SLACK_MS),
            });
        let mut recent: Vec<String> = before
            .iter()
            .rev()
            .filter(|e| e.kind == "PreToolUse")
            .take(context)
            .map(|e| summary(&e.kind, e.tool.as_deref(), &e.payload, 120))
            .collect();
        recent.reverse();
        (goal, recent, transcript)
    };
    let mut out = vec![];
    let mut make = |kind, session: &str, ts: i64, detail: Value| {
        let (goal, recent, transcript) = ctx(session, ts);
        out.push(Episode {
            kind,
            session: session.to_owned(),
            ts_ms: ts,
            goal,
            recent,
            detail,
            transcript,
        });
    };

    let mut session: Option<&str> = None;
    let mut open_turn = false;
    for e in events {
        if session != Some(e.session.as_str()) {
            session = Some(&e.session);
            open_turn = false;
        }
        let input = e.payload.get("tool_input").cloned().unwrap_or(Value::Null); // events without a tool
        match e.kind.as_str() {
            "UserPromptSubmit" => {
                if open_turn {
                    // The human's words while the agent ran: the correction itself.
                    make("cut_in", &e.session, e.ts_ms, json!({ "said": prompt(e) }));
                }
                open_turn = true;
            }
            "Stop" | "StopFailure" => open_turn = false,
            "PermissionDenied" => make(
                "denied",
                &e.session,
                e.ts_ms,
                json!({ "tool": e.tool, "input": input }),
            ),
            "PostToolUseFailure" => make(
                "tool_failure",
                &e.session,
                e.ts_ms,
                json!({ "tool": e.tool, "input": input, "error": cut(e.payload.get("error")) }),
            ),
            _ => {}
        }
    }
    for r in reworks {
        make(
            "rework",
            &r.session,
            r.ts_ms,
            json!({ "path": r.path, "added": r.added, "removed": r.removed, "diff": r.diff }),
        );
    }
    for l in labels {
        make(
            "label",
            &l.session,
            l.verdict_ts_ms,
            json!({ "jig": l.jig, "verdict": l.verdict_id, "evidence": l.evidence, "label": l.label,
                    "note": l.note, "labelled_ts_ms": l.ts_ms }),
        );
    }
    out.sort_by_key(|e| e.ts_ms);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: i64, kind: &str, tool: Option<&str>, payload: Value) -> Ev {
        Ev {
            session: "s".into(),
            ts_ms: ts,
            kind: kind.into(),
            tool: tool.map(Into::into),
            payload,
        }
    }

    #[test]
    fn iso_utc_matches_the_transcript_form() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        // Reference from `date -u -r 1794589880`.
        assert_eq!(iso_utc(1_794_589_880_245), "2026-11-13T17:11:20.245Z");
    }

    #[test]
    fn episode_points_at_its_turn_in_the_transcript() {
        let t = json!({"transcript_path": "/x/s.jsonl"});
        let mut p = t.clone();
        p["prompt"] = json!("goal");
        let events = vec![
            ev(1_000, "SessionStart", None, t.clone()),
            ev(5_000, "UserPromptSubmit", None, p),
            ev(6_000, "PermissionDenied", Some("Bash"), t),
        ];
        let eps = episodes(&events, &[], &[], 5);
        assert_eq!(
            eps[0].transcript,
            Some(TranscriptWindow {
                path: "/x/s.jsonl".into(),
                from: iso_utc(5_000 - WINDOW_SLACK_MS),
                to: iso_utc(6_000 + WINDOW_SLACK_MS)
            })
        );
    }

    #[test]
    fn long_error_is_cut() {
        let long = "e".repeat(DETAIL_MAX_CHARS + 10);
        let v = cut(Some(&json!(long)));
        assert!(
            v.as_str()
                .unwrap()
                .ends_with("(cut; full text in the transcript)")
        );
        assert_eq!(cut(None), Value::Null);
    }

    #[test]
    fn cut_in_carries_the_goal_before_it_and_the_words_said() {
        let events = vec![
            ev(
                1,
                "UserPromptSubmit",
                None,
                json!({"prompt": "fix the tamper guard"}),
            ),
            ev(
                2,
                "PreToolUse",
                Some("Edit"),
                json!({"tool_input": {"file_path": "/r/jig.rs"}}),
            ),
            ev(
                3,
                "AgentMessage",
                None,
                json!({"prompt": "<agent-message> report"}),
            ),
            ev(
                4,
                "PreToolUse",
                Some("Bash"),
                json!({"tool_input": {"command": "cargo test"}}),
            ),
            ev(
                5,
                "UserPromptSubmit",
                None,
                json!({"prompt": "what I want is a jig for runaway agents"}),
            ),
            ev(6, "Stop", None, json!({})),
        ];
        let eps = episodes(&events, &[], &[], 1);
        assert_eq!(eps.len(), 1);
        let e = &eps[0];
        assert_eq!(e.kind, "cut_in");
        assert_eq!(e.goal.as_deref(), Some("fix the tamper guard"));
        assert_eq!(e.recent, vec!["Bash: cargo test"]);
        assert_eq!(e.detail["said"], "what I want is a jig for runaway agents");
    }

    #[test]
    fn rework_and_label_get_the_context_of_their_turn() {
        let events = vec![
            ev(
                1,
                "UserPromptSubmit",
                None,
                json!({"prompt": "write a README"}),
            ),
            ev(
                2,
                "PreToolUse",
                Some("Write"),
                json!({"tool_input": {"file_path": "/r/README.md"}}),
            ),
            ev(3, "Stop", None, json!({})),
            ev(
                9,
                "UserPromptSubmit",
                None,
                json!({"prompt": "done editing"}),
            ),
        ];
        let reworks = vec![ReworkRow {
            session: "s".into(),
            path: "/r/README.md".into(),
            ts_ms: 9,
            added: 1,
            removed: 1,
            diff: Some("-a\n+b\n".into()),
        }];
        let labels = vec![LabelRow {
            ts_ms: 20,
            verdict_ts_ms: 2,
            session: "s".into(),
            verdict_id: "v".into(),
            jig: "tamper-guard".into(),
            evidence: "e".into(),
            label: "wrong".into(),
            note: None,
        }];
        let eps = episodes(&events, &reworks, &labels, 5);
        let kinds: Vec<_> = eps.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["label", "rework"]);
        assert_eq!(eps[1].goal.as_deref(), Some("write a README"));
        assert_eq!(eps[1].recent, vec!["Write: /r/README.md"]);
        assert_eq!(eps[1].detail["diff"], "-a\n+b\n");
        assert_eq!(eps[0].recent, Vec::<String>::new()); // the verdict came at the call itself
    }
}
