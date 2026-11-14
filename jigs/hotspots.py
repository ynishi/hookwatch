#!/usr/bin/env python3
"""Jig: scripts the agent wrote and ran, bundled by what they are for. A bundle
that several sessions ran successfully several times is a candidate for a
canonical tool. Port of src/script.rs (same behavior).

Events come from `$HOOKWATCH_BIN events --json --repo <repo> [--until-ms <ms>]`
(one JSON per line, oldest first).

Hook (no args): stdin is the hook payload. Exit 0: no candidate. Exit 2: stderr
is one line `candidates: <keys, sorted, ", "-joined>`.
Report: `--report [--json] [--repo PATH]` prints the bundle table, exit 0.
"""
import json
import os
import re
import subprocess
import sys

MIN_SESSIONS = 2
MIN_OK_RUNS = 3

RUN = re.compile(
    r"(?:^|[;&|(]\s*)(?:(?:bash|sh|zsh|python3?)\s+(?:-\S+\s+)*)?"
    r"((?:\./|/|~/)?[\w./@-]+\.(?:sh|py))\b"
)
TAIL = re.compile(r"[-_]?v?\d+\Z")


def script_of_command(cmd):
    m = RUN.search(cmd.lstrip())
    return m.group(1) if m else None


def bundle_key(path):
    name = path.rsplit("/", 1)[-1]
    stem = name.rpartition(".")[0] if "." in name else name
    stem = stem.lower().replace("_", "-")
    key = TAIL.sub("", stem, count=1)
    return key if key else stem


def outcome(events, i):
    """The next result of the same call: ran / denied / failed / cut_in / no_post."""
    call = events[i]
    inp = call["payload"].get("tool_input")
    for e in events[i + 1:]:
        if e["session"] != call["session"]:
            break
        same = e["tool"] == call["tool"] and e["payload"].get("tool_input") == inp
        k = e["kind"]
        if k == "PostToolUse" and same:
            return "ran"
        if k == "PermissionDenied" and same:
            return "denied"
        if k == "PostToolUseFailure" and same:
            return "failed"
        if k == "UserPromptSubmit":
            return "cut_in"
        if k in ("Stop", "StopFailure"):
            break
    return "no_post"


def bundles(events):
    """`events` ordered by (session, ts_ms)."""
    by = {}
    for i, e in enumerate(events):
        if e["kind"] != "PreToolUse":
            continue
        inp = e["payload"].get("tool_input")

        def s(k):
            v = inp.get(k) if isinstance(inp, dict) else None
            return v if isinstance(v, str) else None

        tool = e["tool"]
        if tool == "Bash":
            path = script_of_command(s("command")) if s("command") is not None else None
            if path is None:
                continue
            is_run = True
        elif tool in ("Write", "Edit", "MultiEdit"):
            path = s("file_path")
            if path is None or not (path.endswith(".py") or path.endswith(".sh")):
                continue
            is_run = False
        else:
            continue
        a = by.setdefault(
            bundle_key(path),
            {"paths": set(), "sessions": set(), "writes": 0, "runs": 0,
             "ok": 0, "failed": 0, "last": 0},
        )
        a["paths"].add(path)
        a["sessions"].add(e["session"])
        a["last"] = max(a["last"], e["ts_ms"])
        if is_run:
            a["runs"] += 1
            o = outcome(events, i)
            if o == "ran":
                a["ok"] += 1
            elif o == "failed":
                a["failed"] += 1
        else:
            a["writes"] += 1
    out = [
        {"key": k, "paths": sorted(a["paths"]), "writes": a["writes"], "runs": a["runs"],
         "ok": a["ok"], "failed": a["failed"], "sessions": len(a["sessions"]),
         "last_ts_ms": a["last"]}
        for k, a in by.items()
    ]
    out.sort(key=lambda b: (-b["ok"], -b["runs"], b["key"]))
    return out


def is_candidate(b):
    return b["sessions"] >= MIN_SESSIONS and b["ok"] >= MIN_OK_RUNS


def repo_root(start):
    cur = os.path.abspath(start)
    d = cur
    while True:
        if os.path.exists(os.path.join(d, ".git")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return cur
        d = parent


def load_events(repo, until_ms=None):
    cmd = [os.environ.get("HOOKWATCH_BIN", "hookwatch"), "events", "--json", "--repo", repo]
    if until_ms:
        cmd += ["--until-ms", until_ms]
    r = subprocess.run(cmd, check=True, capture_output=True, text=True)
    events = [json.loads(line) for line in r.stdout.splitlines() if line.strip()]
    events.sort(key=lambda e: (e["session"], e["ts_ms"]))  # stable
    return events


def report(repo, as_json):
    root = repo_root(repo)
    bs = bundles(load_events(root))
    if as_json:
        print(json.dumps([dict(b, candidate=is_candidate(b)) for b in bs], indent=2))
        return 0
    print(f"scripts of {root} (candidate: {MIN_SESSIONS}+ sessions and {MIN_OK_RUNS}+ successful runs)")
    print(f"{'bundle':<28} {'runs':>5} {'ok':>4} {'failed':>6} {'writes':>6} {'sessions':>8}  status")
    for b in bs:
        st = "candidate" if is_candidate(b) else "-"
        print(f"{b['key']:<28} {b['runs']:>5} {b['ok']:>4} {b['failed']:>6} {b['writes']:>6} {b['sessions']:>8}  {st}")
    return 0


def main(argv):
    if "--report" in argv:
        repo = argv[argv.index("--repo") + 1] if "--repo" in argv else "."
        return report(repo, "--json" in argv)
    payload = json.load(sys.stdin)
    root = repo_root(payload["cwd"])
    keys = sorted(b["key"] for b in bundles(load_events(root, os.environ.get("HOOKWATCH_EVENT_TS_MS"))) if is_candidate(b))
    if not keys:
        return 0
    print("candidates: " + ", ".join(keys), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
