//! Stopwatch: friction measures computed from the event stream. Pure.

use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq)]
pub struct Friction {
    pub sessions: usize,
    /// Prompts the human wrote (subagent hand-backs are not counted).
    pub turns: usize,
    /// A human prompt came while the agent's turn had no Stop yet: the human
    /// cut in, by Esc / Ctrl+C or by a message sent mid-turn. Hooks cannot
    /// tell the two apart; both are the human stepping in while the agent runs.
    pub cut_ins: usize,
    /// Messages from subagents, delivered as UserPromptSubmit.
    pub agent_messages: usize,
    pub permission_requests: usize,
    /// ms from a PermissionRequest to the next event of the same session:
    /// the time the agent stood waiting for the human (チョコ停).
    pub waits_ms: Vec<i64>,
    /// Same, for questions the agent asked (AskUserQuestion): the human was
    /// asked to decide, not to permit.
    pub question_waits_ms: Vec<i64>,
    pub denied: usize,
    pub tool_failures: usize,
    pub stop_failures: usize,
    pub by_event: BTreeMap<String, usize>,
}

impl Friction {
    pub fn wait_total_ms(&self) -> i64 {
        self.waits_ms.iter().sum()
    }

    pub fn wait_p50_ms(&self) -> Option<i64> {
        let mut w = self.waits_ms.clone();
        w.sort_unstable();
        w.get(w.len() / 2).copied()
    }

    pub fn wait_max_ms(&self) -> Option<i64> {
        self.waits_ms.iter().max().copied()
    }
}

/// The kind of an event for the stopwatch: the hook event name, refined
/// where one name carries two kinds of friction.
pub fn classify(event: &str, tool: Option<&str>, payload: &serde_json::Value) -> String {
    match (event, tool) {
        ("UserPromptSubmit", _) => {
            let prompt = payload.get("prompt").and_then(|v| v.as_str());
            if prompt.is_some_and(|p| p.trim_start().starts_with("<agent-message")) {
                "AgentMessage".into()
            } else {
                event.into()
            }
        }
        ("PermissionRequest", Some("AskUserQuestion")) => "QuestionAsked".into(),
        _ => event.into(),
    }
}

enum Waiting {
    Permission(i64),
    Question(i64),
}

/// `stream` is (session, ts_ms, kind from `classify`), ordered by session then time.
pub fn measure(stream: &[(String, i64, String)]) -> Friction {
    let mut f = Friction::default();
    let mut session: Option<&str> = None;
    // Per session state.
    let mut open_turn = false;
    let mut waiting: Option<Waiting> = None;

    for (s, ts, ev) in stream {
        if session != Some(s.as_str()) {
            session = Some(s);
            f.sessions += 1;
            open_turn = false;
            waiting = None;
        }
        *f.by_event.entry(ev.clone()).or_insert(0) += 1;
        match waiting.take() {
            Some(Waiting::Permission(start)) => f.waits_ms.push(ts - start),
            Some(Waiting::Question(start)) => f.question_waits_ms.push(ts - start),
            None => {}
        }
        match ev.as_str() {
            "UserPromptSubmit" => {
                f.turns += 1;
                if open_turn {
                    f.cut_ins += 1;
                }
                open_turn = true;
            }
            "AgentMessage" => f.agent_messages += 1,
            "QuestionAsked" => waiting = Some(Waiting::Question(*ts)),
            "Stop" => open_turn = false,
            "StopFailure" => {
                f.stop_failures += 1;
                open_turn = false;
            }
            "PermissionRequest" => {
                f.permission_requests += 1;
                waiting = Some(Waiting::Permission(*ts));
            }
            "PermissionDenied" => f.denied += 1,
            "PostToolUseFailure" => f.tool_failures += 1,
            _ => {}
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(v: &[(&str, i64, &str)]) -> Vec<(String, i64, String)> {
        v.iter()
            .map(|(s, t, e)| (s.to_string(), *t, e.to_string()))
            .collect()
    }

    #[test]
    fn cut_in_is_a_human_prompt_without_stop_before_it() {
        let f = measure(&st(&[
            ("a", 0, "UserPromptSubmit"),
            ("a", 1, "PreToolUse"),
            ("a", 2, "AgentMessage"), // a subagent reporting: not the human
            ("a", 3, "UserPromptSubmit"), // cut in
            ("a", 4, "Stop"),
            ("a", 5, "UserPromptSubmit"),
            ("a", 6, "Stop"),
        ]));
        assert_eq!((f.turns, f.cut_ins, f.agent_messages), (3, 1, 1));
    }

    /// Seen on 2026-11-14: a subagent hand-back came as UserPromptSubmit
    /// and was counted as an interrupted turn.
    #[test]
    fn classify_splits_agent_messages_and_questions() {
        let p = serde_json::json!({"prompt": "<agent-message from=\"x\"> report"});
        assert_eq!(classify("UserPromptSubmit", None, &p), "AgentMessage");
        let p = serde_json::json!({"prompt": "OK, 修正してみて"});
        assert_eq!(classify("UserPromptSubmit", None, &p), "UserPromptSubmit");
        let p = serde_json::json!({});
        assert_eq!(
            classify("PermissionRequest", Some("AskUserQuestion"), &p),
            "QuestionAsked"
        );
        assert_eq!(
            classify("PermissionRequest", Some("Bash"), &p),
            "PermissionRequest"
        );
    }

    #[test]
    fn question_wait_is_not_a_permission_wait() {
        let f = measure(&st(&[
            ("a", 0, "QuestionAsked"),
            ("a", 9000, "PostToolUse"),
        ]));
        assert_eq!(
            (f.waits_ms.len(), f.question_waits_ms.clone()),
            (0, vec![9000])
        );
    }

    #[test]
    fn wait_is_time_to_next_event_in_same_session() {
        let f = measure(&st(&[
            ("a", 100, "PermissionRequest"),
            ("a", 4100, "PostToolUse"),
            ("b", 0, "PermissionRequest"), // session ends: no wait counted
            ("c", 0, "UserPromptSubmit"),
        ]));
        assert_eq!(f.waits_ms, vec![4000]);
        assert_eq!(f.sessions, 3);
        assert_eq!(f.permission_requests, 2);
        // A new session does not continue an open turn of the old one.
        assert_eq!(f.cut_ins, 0);
    }
}
