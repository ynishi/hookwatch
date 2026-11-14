import importlib.util
import os
import unittest

_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "hotspots.py")
_spec = importlib.util.spec_from_file_location("hotspots", _path)
hs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hs)


def ev(session, ts, kind, tool, payload):
    return {"session": session, "ts_ms": ts, "kind": kind, "tool": tool, "payload": payload}


class HotspotsTest(unittest.TestCase):
    def test_script_of_command_finds_the_script_run(self):
        cases = [
            ("bash /tmp/sp/push-main.sh", "/tmp/sp/push-main.sh"),
            ("python3 -I /r/jigs/x.py arg", "/r/jigs/x.py"),
            ("cd /r && ./tools/run_gradle.py --dry-run", "./tools/run_gradle.py"),
            ("sh build.sh", "build.sh"),
            ("ls -la", None),
            ("cat notes.sh", None),
        ]
        for cmd, want in cases:
            self.assertEqual(hs.script_of_command(cmd), want, cmd)

    def test_bundle_key_drops_versions_and_numbers(self):
        self.assertEqual(hs.bundle_key("/sp/commit-msg3.sh"), "commit-msg")
        self.assertEqual(hs.bundle_key("/sp/smoke_v2.py"), "smoke")
        self.assertEqual(hs.bundle_key("/sp/Smoke-Rework.sh"), "smoke-rework")
        self.assertEqual(hs.bundle_key("/sp/2.sh"), "2")

    def test_bundles_count_runs_outcomes_and_sessions_and_mark_candidates(self):
        def run(p):
            return {"tool_input": {"command": f"bash {p}"}}

        events = [ev("a", 1, "PreToolUse", "Write", {"tool_input": {"file_path": "/sp/push-main.sh"}})]
        t = 2
        for s, p, result in [
            ("a", "/sp/push-main.sh", "PostToolUse"),
            ("a", "/sp/push-main2.sh", "PostToolUseFailure"),
            ("b", "/sp/push_main.sh", "PostToolUse"),
            ("b", "/sp/push-main.sh", "PostToolUse"),
        ]:
            events.append(ev(s, t, "PreToolUse", "Bash", run(p)))
            events.append(ev(s, t + 1, result, "Bash", run(p)))
            t += 2
        events.sort(key=lambda e: (e["session"], e["ts_ms"]))
        b = hs.bundles(events)
        self.assertEqual(len(b), 1)
        p = b[0]
        self.assertEqual(
            (p["key"], p["writes"], p["runs"], p["ok"], p["failed"], p["sessions"]),
            ("push-main", 1, 4, 3, 1, 2),
        )
        self.assertEqual(len(p["paths"]), 3)
        self.assertTrue(hs.is_candidate(p))
        self.assertFalse(hs.is_candidate(dict(p, sessions=1)))


if __name__ == "__main__":
    unittest.main()
