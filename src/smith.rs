//! What Jig Smith reads: an excerpt of the conversation around an episode.
//! Pure: the caller reads the transcript file and hands in its lines.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, PartialEq)]
pub struct Turn {
    pub who: &'static str,
    pub at: String,
    pub text: String,
}

const TEXT_MAX: usize = 400;
const TURNS_MAX: usize = 20;

fn cut(s: &str) -> String {
    let t: String = s.chars().take(TEXT_MAX).collect();
    if s.chars().count() > TEXT_MAX {
        format!("{t}…")
    } else {
        t
    }
}

/// The human's and the agent's words with `from <= timestamp <= to`
/// (ISO strings in the transcript's own form compare in time order). Tool
/// calls and their results are left out: the episode already lists them.
/// The last `TURNS_MAX` turns are kept. A line that is not JSON is an error.
pub fn excerpt<'a>(
    lines: impl Iterator<Item = &'a str>,
    from: &str,
    to: &str,
) -> anyhow::Result<Vec<Turn>> {
    let mut out = vec![];
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("transcript line {}: {e}", n + 1))?;
        let Some(at) = v.get("timestamp").and_then(|t| t.as_str()) else {
            continue;
        }; // meta lines carry no time
        if at < from || at > to {
            continue;
        }
        let content = v.pointer("/message/content");
        let turn = match (v.get("type").and_then(|t| t.as_str()), content) {
            (Some("user"), Some(Value::String(s))) => Some(("human", s.clone())),
            (Some("assistant"), Some(Value::Array(parts))) => {
                let text: Vec<&str> = parts
                    .iter()
                    .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect();
                if text.is_empty() {
                    None
                } else {
                    Some(("agent", text.join("\n")))
                }
            }
            _ => None, // tool results, attachments, system lines
        };
        if let Some((who, text)) = turn {
            out.push(Turn {
                who,
                at: at.to_owned(),
                text: cut(&text),
            });
        }
    }
    let skip = out.len().saturating_sub(TURNS_MAX);
    Ok(out.into_iter().skip(skip).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_keeps_words_in_the_window_only() {
        let lines = [
            r#"{"type":"user","timestamp":"2026-01-01T00:00:01.000Z","message":{"content":"before"}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:05.000Z","message":{"content":"fix it"}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:06.000Z","message":{"content":[{"type":"tool_use","name":"Edit"},{"type":"text","text":"done"}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:07.000Z","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"summary"}"#,
            "",
            r#"{"type":"user","timestamp":"2026-01-01T00:00:09.000Z","message":{"content":"after"}}"#,
        ];
        let t = excerpt(
            lines.into_iter(),
            "2026-01-01T00:00:02.000Z",
            "2026-01-01T00:00:08.000Z",
        )
        .unwrap();
        let got: Vec<_> = t.iter().map(|t| (t.who, t.text.as_str())).collect();
        assert_eq!(got, vec![("human", "fix it"), ("agent", "done")]);
        assert!(excerpt(["not json"].into_iter(), "a", "z").is_err());
    }
}
