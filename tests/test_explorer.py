import http.client, json, os, subprocess, sys, tempfile, threading, time, unittest
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


def try_symlink(link: Path, target: Path, is_dir=False):
    try:
        os.symlink(target, link, target_is_directory=is_dir)
    except (OSError, NotImplementedError) as e:  # Windows without developer mode / admin
        if is_dir and os.name == "nt":            # a junction needs no special rights and must be refused just the same
            r = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)], capture_output=True)
            if r.returncode == 0:
                return
        raise unittest.SkipTest(f"cannot create symlinks here: {e}")


class Rename(unittest.TestCase):
    def test_markdown_links_rewritten_relative_to_each_note(self):
        v = make_vault({
            "A/target.md": "x",
            "A/sib.md": "[t](target.md) and [h](target.md#Head)",
            "B/far.md": "[t](../A/target.md) [u](<../A/target.md>) [ext](https://e.com/target.md)",
            "root.md": "[t](/A/target.md) [[target]]",
        })
        idx = ox.VaultIndex(v)
        ox.rename_note(v, idx, "A/target", "C/moved")
        self.assertEqual((v / "A/sib.md").read_text(), "[t](../C/moved.md) and [h](../C/moved.md#Head)")
        self.assertEqual((v / "B/far.md").read_text(),
                         "[t](../C/moved.md) [u](<../C/moved.md>) [ext](https://e.com/target.md)")
        self.assertIn("[[moved]]", (v / "root.md").read_text())
        self.assertIn("C/moved.md", (v / "root.md").read_text())

    def test_moved_note_keeps_its_own_relative_links_working(self):
        v = make_vault({"A/n.md": "[s](sib.md)", "A/sib.md": "s"})
        ox.rename_note(v, ox.VaultIndex(v), "A/n", "B/n")
        self.assertEqual((v / "B/n.md").read_text(), "[s](../A/sib.md)")


class Safety(unittest.TestCase):
    def test_symlinked_note_is_ignored_and_refused(self):
        outside = make_vault({"secret.md": "[[x]] outside"})
        v = make_vault({"x.md": "x"})
        try_symlink(v / "link.md", outside / "secret.md")
        res = ox.VaultIndex(v).build()
        self.assertEqual([n["id"] for n in res["notes"]], ["x"])
        for fn in (lambda: ox.read_note(v, "link"), lambda: ox.note_path(v, "link")):
            with self.assertRaises(ValueError):
                fn()
        ox.rename_note(v, ox.VaultIndex(v), "x", "y")   # must not rewrite the file behind the link
        self.assertEqual((outside / "secret.md").read_text(), "[[x]] outside")

    def test_symlinked_folder_is_ignored_and_refused(self):
        outside = make_vault({"in.md": "hi"})
        v = make_vault({"x.md": "x"})
        try_symlink(v / "dir", outside, is_dir=True)
        self.assertEqual([n["id"] for n in ox.VaultIndex(v).build()["notes"]], ["x"])
        with self.assertRaises(ValueError):
            ox.create_note(v, ox.VaultIndex(v), "dir/new", "boo")
        self.assertFalse((outside / "new.md").exists())

    def test_oversized_notes_skipped_and_refused(self):
        v = make_vault({"big.md": "x" * 100, "small.md": "x"})
        old = ox.MAX_NOTE_BYTES
        ox.MAX_NOTE_BYTES = 50
        try:
            self.assertEqual([n["id"] for n in ox.VaultIndex(v).build()["notes"]], ["small"])
            with self.assertRaises(ValueError):
                ox.read_note(v, "big")
            with self.assertRaises(ValueError):
                ox.save_note(v, ox.VaultIndex(v), "small", "y" * 60)
        finally:
            ox.MAX_NOTE_BYTES = old


class Watch(unittest.TestCase):
    def test_version_changes_only_when_vault_changes(self):
        v = make_vault({"a.md": "one"})
        idx = ox.VaultIndex(v)
        idx.build()
        v0 = idx.version
        idx.build(force=True)
        self.assertEqual(idx.version, v0)
        (v / "b.md").write_text("two", encoding="utf-8")
        idx.build(force=True)
        self.assertEqual(idx.version, v0 + 1)
        self.assertIn("rev", idx.build()["notes"][0])

    def test_server_pushes_change_event(self):
        v = make_vault({"a.md": "one"})
        srv = ox.make_server(v, 0, watch=0.1)
        port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            c.request("GET", "/api/events")
            r = c.getresponse()
            self.assertEqual(r.status, 200)
            self.assertEqual(r.readline(), b"event: hello\n")
            time.sleep(0.3)
            (v / "b.md").write_text("two", encoding="utf-8")
            lines = []
            while b"event: change\n" not in lines:
                lines.append(r.readline())
                self.assertLess(len(lines), 50)
            c.close()
            bad = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            bad.request("GET", "/api/events", headers={"Host": "evil.example"})
            self.assertEqual(bad.getresponse().status, 403)
        finally:
            srv.shutdown()
            srv.server_close()


class Theme(unittest.TestCase):
    def test_theme_and_snippets_are_embedded(self):
        v = make_vault({
            "n.md": "a note that mentions /*__OBSCSS__*/ and </style>",
            ".obsidian/appearance.json": json.dumps({"cssTheme": "Neat", "theme": "moonstone",
                                                     "enabledCssSnippets": ["wide", "missing", "../evil"]}),
            ".obsidian/themes/Neat/theme.css": ".theme-light{--background-primary:#abcdef}</style><script>",
            ".obsidian/snippets/wide.css": "body{--file-line-width:900px}",
            ".obsidian/evil.css": "nope",
        })
        obs = ox.obsidian_css(v)
        self.assertEqual(obs["mode"], "light")
        self.assertIn("#abcdef", obs["css"])
        self.assertIn("--file-line-width", obs["css"])
        self.assertNotIn("nope", obs["css"])
        html = ox.page(v, False, theme=True)
        self.assertIn("#abcdef", html)
        self.assertNotIn("</style><script>", html)            # theme CSS cannot close the style element
        self.assertIn('window.OBS={"mode": "light"', html)
        self.assertIn("mentions /*__OBSCSS__*/", html)         # vault text is not treated as a placeholder
        self.assertEqual(html.count("--file-line-width"), 1)

    def test_no_theme_by_default(self):
        v = make_vault({"n.md": "x", ".obsidian/snippets/s.css": "body{}",
                        ".obsidian/appearance.json": '{"enabledCssSnippets":["s"]}'})
        html = ox.page(v, False)
        self.assertIn("window.OBS=null", html)
        self.assertIn('<style id="obscss"></style>', html)
        self.assertNotIn("__", html.split('<script type="application/json"')[0].split("<style id=\"obscss\">")[1][:5])


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
