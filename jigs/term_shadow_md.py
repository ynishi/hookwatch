#!/usr/bin/env python3
"""Jig: in Markdown prose, the lifecycle term is written "Shadow".

Source: rework of docs/jig-authoring.md on 2026-11-14 (the human rewrote
"shadow で入れる" as "Shadow で確定する"). Only prose counts: code spans and
fenced blocks keep the lowercase value ("lifecycle": "shadow").

Run by hookwatch (rule kind "command") with the hook payload on stdin.
Exit 0: no hit. Exit 2: hit, the reason on stderr.
"""
import json
import re
import sys

WORD = re.compile(r"(?<![\w\"'`])shadow(?![\w\"'`])")
FENCE = re.compile(r"```.*?```", re.S)
CODE_SPAN = re.compile(r"`[^`\n]*`")


def prose(text: str) -> str:
    """The text with code blanked out. Newlines are kept, so line numbers
    still match the file."""
    no_fences = FENCE.sub(lambda m: "\n" * m.group().count("\n"), text)
    return CODE_SPAN.sub("", no_fences)


def added_text(tool: str, inp: dict) -> list[str]:
    if tool == "Write":
        return [inp["content"]]
    if tool == "Edit":
        return [inp["new_string"]]
    if tool == "MultiEdit":
        return [e["new_string"] for e in inp["edits"]]
    return []


def main() -> int:
    payload = json.load(sys.stdin)
    tool = payload.get("tool_name", "")
    inp = payload.get("tool_input", {})
    path = inp.get("file_path", "")
    if not path.endswith(".md"):
        return 0
    for text in added_text(tool, inp):
        p = prose(text)
        m = WORD.search(p)
        if m:
            line = p[: m.start()].count("\n") + 1
            print(f"lowercase 'shadow' in prose at line {line} of {path}; write 'Shadow'", file=sys.stderr)
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
