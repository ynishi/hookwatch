//! Rework: the human changing, between two turns, a file the agent left.
//! Which files the agent edited and how many lines changed are pure; the
//! hook reads the files.

use std::collections::BTreeMap;

const FILE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The file a PostToolUse payload edited, if its tool edits files. A file
/// tool without a path is an error: the payload is not what Claude Code sends.
pub fn edited_path(payload: &serde_json::Value) -> anyhow::Result<Option<String>> {
    let Some(tool) = payload.get("tool_name").and_then(|v| v.as_str()) else {
        anyhow::bail!("PostToolUse payload has no tool_name");
    };
    if !FILE_TOOLS.contains(&tool) {
        return Ok(None);
    }
    let input = &payload["tool_input"];
    let path = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("{tool} payload has no file_path"))?;
    Ok(Some(path.to_owned()))
}

/// (added, removed) lines from `old` to `new`, as a multiset of lines: a
/// moved line counts as nothing, a changed line as one added and one removed.
pub fn line_delta(old: &str, new: &str) -> (usize, usize) {
    let mut count: BTreeMap<&str, i64> = BTreeMap::new();
    for l in old.lines() {
        *count.entry(l).or_insert(0) -= 1;
    }
    for l in new.lines() {
        *count.entry(l).or_insert(0) += 1;
    }
    let added = count
        .values()
        .filter(|c| **c > 0)
        .map(|c| *c as usize)
        .sum();
    let removed = count
        .values()
        .filter(|c| **c < 0)
        .map(|c| (-*c) as usize)
        .sum();
    (added, removed)
}

/// Above this many cells the LCS table is not built; the diff lists the
/// removed and added lines without positions instead.
const LCS_LIMIT: usize = 4_000_000;
const CONTEXT: usize = 2;

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Same,
    Del,
    Ins,
}

/// A unified-style diff of `old` → `new` with 2 lines of context, for a
/// person or an AI to read what the human changed. Empty when equal.
pub fn diff_text(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a == b {
        return String::new();
    }
    if a.len().saturating_mul(b.len()) > LCS_LIMIT {
        return coarse(&a, &b);
    }
    // lcs[i][j] = LCS length of a[i..] and b[j..]
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    // (op, text, old line no, new line no), 1-based
    let mut ops: Vec<(Op, &str, usize, usize)> = vec![];
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((Op::Same, a[i], i + 1, j + 1));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Deletions first, as `diff -u` shows them.
            ops.push((Op::Del, a[i], i + 1, j + 1));
            i += 1;
        } else {
            ops.push((Op::Ins, b[j], i + 1, j + 1));
            j += 1;
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|k| ops[*k].0 != Op::Same).collect();
    let near = |k: usize| changed.iter().any(|c| c.abs_diff(k) <= CONTEXT);
    let mut out = String::new();
    let mut last_shown: Option<usize> = None;
    for (k, (op, text, oi, nj)) in ops.iter().enumerate() {
        if !near(k) {
            continue;
        }
        if last_shown != Some(k.wrapping_sub(1)) {
            out.push_str(&format!("@@ -{oi} +{nj} @@\n"));
        }
        let sign = match op {
            Op::Same => ' ',
            Op::Del => '-',
            Op::Ins => '+',
        };
        out.push(sign);
        out.push_str(text);
        out.push('\n');
        last_shown = Some(k);
    }
    out
}

fn coarse(a: &[&str], b: &[&str]) -> String {
    let mut count: BTreeMap<&str, i64> = BTreeMap::new();
    for l in a {
        *count.entry(l).or_insert(0) -= 1;
    }
    for l in b {
        *count.entry(l).or_insert(0) += 1;
    }
    let mut out = String::from("@@ (large file: changed lines without positions) @@\n");
    for (l, c) in &count {
        let sign = if *c < 0 { '-' } else { '+' };
        for _ in 0..c.unsigned_abs() {
            out.push(sign);
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_shows_changed_lines_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "1\n2\n3\nfour\n5\n6\n7\n8\n9\nten\n";
        assert_eq!(
            diff_text(old, new),
            "@@ -2 +2 @@\n 2\n 3\n-4\n+four\n 5\n 6\n@@ -8 +8 @@\n 8\n 9\n+ten\n"
        );
        assert_eq!(diff_text("a\n", "a\n"), "");
    }

    /// The README heading fix of 2026-11-14 (+2 -2 in the stats).
    #[test]
    fn diff_of_a_heading_rename() {
        let d = diff_text("x\n## 何をするか\ny\n", "x\n## What’s hookwatch?\ny\n");
        assert_eq!(
            d,
            "@@ -1 +1 @@\n x\n-## 何をするか\n+## What’s hookwatch?\n y\n"
        );
    }

    #[test]
    fn delta_counts_changed_lines_and_ignores_moves() {
        assert_eq!(line_delta("a\nb\nc\n", "a\nb\nc\n"), (0, 0));
        assert_eq!(line_delta("a\nb\nc\n", "c\na\nb\n"), (0, 0));
        assert_eq!(line_delta("a\nb\n", "a\nB\nd\n"), (2, 1));
        assert_eq!(line_delta("a\nb\n", ""), (0, 2));
    }

    #[test]
    fn edited_path_for_file_tools_only() {
        let p = json!({"tool_name": "Edit", "tool_input": {"file_path": "/r/a.rs"}});
        assert_eq!(edited_path(&p).unwrap().as_deref(), Some("/r/a.rs"));
        let p = json!({"tool_name": "NotebookEdit", "tool_input": {"notebook_path": "/r/n.ipynb"}});
        assert_eq!(edited_path(&p).unwrap().as_deref(), Some("/r/n.ipynb"));
        let p = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
        assert_eq!(edited_path(&p).unwrap(), None);
        let p = json!({"tool_name": "Write", "tool_input": {}});
        assert!(edited_path(&p).is_err());
    }
}
