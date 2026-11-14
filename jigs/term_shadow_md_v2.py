#!/usr/bin/env python3
"""Jig v2: in Markdown prose, the lifecycle term is written "Shadow" when it
is a word in a sentence. Lowercase stays when it is the value itself: in code,
in parentheses "(shadow)", or alone in a table cell "| shadow |".

v1 (term_shadow_md.py) hit README.md line 11 "記録だけする (shadow)" and the
table cell "| shadow |"; the human corrected neither (2026-11-14 rework of
README.md), while they rewrote "shadow で入れる" as "Shadow で確定する" in
docs/jig-authoring.md.

Run by hookwatch (rule kind "command") with the hook payload on stdin.
Exit 0: no hit. Exit 2: hit, the reason on stderr.
"""
import json
import re
import sys

WORD = re.compile(r"(?<![\w\"'`])shadow(?![\w\"'`])")
FENCE = re.compile(r"```.*?```", re.S)
CODE_SPAN = re.compile(r"`[^`\n]*`")
AS_VALUE = re.compile(r"\(\s*shadow\s*\)|\|\s*shadow\s*\|")


def prose(text: str) -> str:
    """Code and value-like uses blanked out. Newlines are kept, so line
    numbers still match the file."""
    no_fences = FENCE.sub(lambda m: "\n" * m.group().count("\n"), text)
    no_code = CODE_SPAN.sub("", no_fences)
    return AS_VALUE.sub("", no_code)


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
            print(f"lowercase 'shadow' in a sentence at line {line} of {path}; write 'Shadow'", file=sys.stderr)
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
