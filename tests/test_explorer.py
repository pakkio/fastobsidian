import json, subprocess, sys, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
import obsidian_explorer as ox  # noqa: E402


def make_vault(files):
    d = Path(tempfile.mkdtemp()).resolve()
    for name, text in files.items():
        p = d / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
    return d


class Core(unittest.TestCase):
    def setUp(self):
        self.v = make_vault({
            "Docker.md": "---\naliases: [Containers]\ntags: [infra]\n---\nbody about whales",
            "a.md": "see [[Containers]] and [[Docker]] and [[nope]]",
            "Other/Containers.md": "real note named containers",
            "b.md": "mentions nginx here",
        })
        self.idx = ox.VaultIndex(self.v)

    def test_alias_resolves_but_real_name_wins(self):
        res = self.idx.build()
        self.assertEqual(res["lookup"]["containers"], "Other/Containers")
        self.assertEqual([u["target"] for u in res["unresolved"]], ["nope"])

    def test_alias_link_when_no_clash(self):
        (self.v / "Other" / "Containers.md").unlink()
        self.idx.invalidate()
        res = self.idx.build()
        self.assertEqual(res["lookup"]["containers"], "Docker")
        self.assertIn("Docker", next(n for n in res["notes"] if n["id"] == "a")["links"])

    def test_search_ranking_and_tag(self):
        ids = [r["id"] for r in self.idx.search("docker")]
        self.assertEqual(ids[0], "Docker")
        self.assertEqual([r["id"] for r in self.idx.search("containers")][:2], ["Other/Containers", "Docker"])
        self.assertEqual([r["id"] for r in self.idx.search("", tag="infra")], ["Docker"])
        self.assertEqual(self.idx.search("nginx")[0]["id"], "b")

    def test_append_and_conflict(self):
        ox.save_note(self.v, self.idx, "b", "more", append=True)
        self.assertTrue((self.v / "b.md").read_text().endswith("here\nmore"))
        with self.assertRaises(ox.Conflict):
            ox.save_note(self.v, self.idx, "b", "x", mtime=1)

    def test_rename_rewrites_links(self):
        ox.rename_note(self.v, self.idx, "Docker", "Whale")
        self.assertIn("[[Whale]]", (self.v / "a.md").read_text())

    def test_traversal_rejected(self):
        for bad in ("../x", "a/../../x", ".hidden/x", "C:/x"):
            with self.assertRaises(ValueError):
                ox.note_path(self.v, bad)

    def test_trash(self):
        ox.trash_note(self.v, self.idx, "b")
        self.assertTrue((self.v / ".trash" / "b.md").exists())


class Mcp(unittest.TestCase):
    def test_stdio(self):
        v = make_vault({"n.md": "hello world"})
        msgs = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "write", "arguments": {"id": "Dir/new", "content": "hi", "tags": ["t"]}}},
            {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "search", "arguments": {"query": "hello"}}},
            {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "read", "arguments": {"id": "../etc"}}},
        ]
        out = subprocess.run([sys.executable, str(ROOT / "obsidian_explorer.py"), str(v), "--mcp"],
                             input="\n".join(map(json.dumps, msgs)) + "\n", capture_output=True, text=True, timeout=30)
        r = {m["id"]: m for m in map(json.loads, out.stdout.splitlines())}
        self.assertEqual(sorted(r), [1, 2, 3, 4, 5])
        self.assertEqual(len(r[2]["result"]["tools"]), 7)
        self.assertTrue((v / "Dir" / "new.md").read_text().startswith("---\ntags:\n  - t\n---\nhi"))
        self.assertIn("n", r[4]["result"]["content"][0]["text"])
        self.assertTrue(r[5]["result"]["isError"])


if __name__ == "__main__":
    unittest.main()
