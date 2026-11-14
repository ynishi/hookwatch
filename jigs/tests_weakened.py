#!/usr/bin/env python3
"""Jig: an edit that weakens a test (fewer assertions or a new skip marker).
Port of TestPatterns::check in src/jig.rs (same patterns, same evidence).

Input: PreToolUse JSON on stdin. Exit 0: no hit (or not an Edit / Write /
MultiEdit of a test). Exit 2: hit, the evidence on stderr.
"""
import json
import re
import sys

TEST_PATH = re.compile(
    r"(^|/)(tests?|__tests__|spec)/|_test\.(go|py|rs)\Z|\.(test|spec)\.[cm]?[jt]sx?\Z|(^|/)test_[^/]+\.py\Z"
)
INLINE_TEST = re.compile(r"#\[test\]|#\[cfg\(test\)\]|#\[tokio::test")
ASSERTION = re.compile(
    r"\bassert\w*!?\s*\(|\bassert\s|\bexpect\s*\(|\.to[A-Z]\w*\s*\(|\bself\.assert\w+\s*\(|\bt\.(Error|Fatal)\w*\s*\(|\brequire\.\w+\s*\("
)
SKIP = re.compile(
    r"#\[ignore\]|@pytest\.mark\.(skip|xfail)|@unittest\.skip|\b(it|test|describe)\.(skip|todo)\s*\(|\bx(it|describe)\s*\(|\bt\.Skip\w*\s*\(|@Disabled\b|pytest\.skip\s*\("
)


def count(rx, text):
    return len(list(rx.finditer(text)))


def edits_of(tool, inp, on_disk):
    """(old, new) pairs. Write has old = file on disk now (or "")."""
    if tool == "Write":
        return [(on_disk, inp["content"])]
    if tool == "Edit":
        return [(inp["old_string"], inp["new_string"])]
    if tool == "MultiEdit":
        return [(e["old_string"], e["new_string"]) for e in inp["edits"]]
    return []


def check(path, edits):
    out = []
    for old, new in edits:
        if not (TEST_PATH.search(path) or INLINE_TEST.search(old)):
            continue
        a_old, a_new = count(ASSERTION, old), count(ASSERTION, new)
        s_old, s_new = count(SKIP, old), count(SKIP, new)
        if a_new < a_old:
            out.append(f"assertions {a_old} -> {a_new}")
        if s_new > s_old:
            out.append(f"skip markers {s_old} -> {s_new}")
    return ", ".join(out) if out else None


def read_or_empty(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except FileNotFoundError:
        return ""


def main():
    payload = json.load(sys.stdin)
    tool = payload.get("tool_name", "")
    if tool not in ("Edit", "Write", "MultiEdit"):
        return 0
    inp = payload["tool_input"]
    path = inp.get("file_path") or inp.get("notebook_path") or ""
    on_disk = read_or_empty(path) if tool == "Write" and path else ""
    evidence = check(path, edits_of(tool, inp, on_disk))
    if evidence is None:
        return 0
    print(evidence, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
