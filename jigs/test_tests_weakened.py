import importlib.util
import os
import unittest

_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tests_weakened.py")
_spec = importlib.util.spec_from_file_location("tests_weakened", _path)
tw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tw)


def run(tool, inp, on_disk=""):
    return tw.check(inp.get("file_path", ""), tw.edits_of(tool, inp, on_disk))


class TestsWeakenedTest(unittest.TestCase):
    def test_sees_removed_assertion_and_new_ignore(self):
        ev = run("Edit", {
            "file_path": "/r/src/lib.rs",
            "old_string": "#[test]\nfn a() { assert_eq!(f(), 1); assert!(g()); }",
            "new_string": "#[test]\n#[ignore]\nfn a() { assert!(g()); }",
        })
        self.assertIn("assertions 2 -> 1", ev)
        self.assertIn("skip markers 0 -> 1", ev)

    def test_ignores_non_test_code_and_added_assertions(self):
        self.assertIsNone(run("Edit", {"file_path": "/r/src/lib.rs", "old_string": "assert!(x);", "new_string": ""}))
        self.assertIsNone(run("Edit", {
            "file_path": "/r/tests/a.rs",
            "old_string": "assert!(x);",
            "new_string": "assert!(x); assert!(y);",
        }))

    def test_write_compares_with_file_on_disk(self):
        ev = run(
            "Write",
            {"file_path": "/r/foo.test.ts", "content": "it('a', () => {})"},
            "it('a', () => { expect(a).toBe(1) })",
        )
        self.assertEqual(ev, "assertions 2 -> 0")


if __name__ == "__main__":
    unittest.main()
