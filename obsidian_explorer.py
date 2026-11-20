#!/usr/bin/env python3
"""Generate a single self-contained HTML explorer (index, search, backlinks, graph)
for an Obsidian vault.

    python obsidian_explorer.py [VAULT_DIR] [-o explorer.html] [--open]
"""
import argparse
import json
import re
import shutil
import sys
import webbrowser
from datetime import datetime
from pathlib import Path
from urllib.parse import unquote

FENCE = re.compile(r"(```|~~~).*?\1", re.S)
INLINE_CODE = re.compile(r"`[^`\n]*`")
WIKI = re.compile(r"(!?)\[\[([^\]|#]*)(#[^\]|]*)?(?:\|[^\]]*)?\]\]")
TAG = re.compile(r"(?<![\w/&#])#([A-Za-z_][\w/-]*)")
FM = re.compile(r"---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|$)", re.S)
MEDIA_EXT = {"png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "pdf", "mp3", "mp4",
             "wav", "ogg", "webm", "mov", "m4a"}
ASSET_EXT = MEDIA_EXT | {"canvas", "base"}


# [text](note.md#h) or [text](<my note.md#h>) -- images (![..]) excluded
MDLINK = re.compile(r"(?<!!)\[[^\]]*\]\((?:<([^>]+?\.md)(?:#[^>]*)?>|([^)\s#]+?\.md)(?:#[^)\s]*)?)(?:\s+\"[^\"]*\")?\)", re.I)


def resolve_md(raw, from_id, lookup):
    """Resolve a markdown-link target (relative to the note, or vault-root) to a note id."""
    if re.match(r"[a-z][a-z0-9+.-]*:", raw, re.I):  # http:, mailto:, ...
        return None
    target = unquote(raw)[:-3]
    base = from_id.rsplit("/", 1)[0].split("/") if "/" in from_id else []
    parts = [] if target.startswith("/") else base[:]
    for seg in target.lstrip("/").split("/"):
        if seg == "..":
            if parts:
                parts.pop()
        elif seg not in ("", "."):
            parts.append(seg)
    return lookup.get("/".join(parts).lower()) or lookup.get(target.lstrip("/").lower())


def parse_frontmatter(text):
    m = FM.match(text) if text.startswith("---") else None
    if not m:
        return {}, text
    fm, key = {}, None
    for line in m.group(1).splitlines():
        item = re.match(r"\s*-\s+(.*)", line)
        kv = re.match(r"([\w-]+):\s*(.*)", line)
        if kv:
            key, val = kv.group(1).lower(), kv.group(2).strip()
            if val.startswith("[") and val.endswith("]"):
                val = [v.strip().strip("'\"") for v in val[1:-1].split(",") if v.strip()]
            elif val and key in ("tags", "tag", "aliases"):
                val = [v.strip().strip("'\"") for v in re.split(r"[,\s]+", val) if v.strip()]
            fm[key] = val if val != "" else []
        elif item and key and isinstance(fm.get(key), list):
            fm[key].append(item.group(1).strip().strip("'\""))
    return fm, text[m.end():]


def parse_note(rel: str, path: str, mtime_ns: int, text: str) -> dict:
    """Parse one note. Link *targets* are extracted here; resolving them needs the whole vault (see VaultIndex)."""
    nid = rel[:-3]
    fm, body = parse_frontmatter(text)
    stripped = INLINE_CODE.sub("", FENCE.sub("", body))
    tags = set()
    for k in ("tags", "tag"):
        v = fm.get(k, [])
        tags.update(str(t).lstrip("#") for t in (v if isinstance(v, list) else [v]) if t)
    tags |= set(TAG.findall(stripped))
    content = body.strip()
    return {
        "id": nid, "title": nid.rsplit("/", 1)[-1], "folder": nid.rsplit("/", 1)[0] if "/" in nid else "",
        "content": content, "lc": content.lower(), "mtime": mtime_ns // 1_000_000_000,
        "words": len(re.findall(r"\w+", body)), "tags": sorted(tags),
        "props": {k: v for k, v in fm.items() if k not in ("tags", "tag")},
        "wiki": [t.strip() for _, t, _ in WIKI.findall(stripped) if t.strip()],
        "mdl": [m.group(1) or m.group(2) for m in MDLINK.finditer(stripped)],
    }


class VaultIndex:
    """Incremental vault index: re-reads only files whose mtime/size changed; caches the resolved result."""

    MIN_RECHECK = 2.0

    def __init__(self, vault: Path):
        import threading
        self.vault = vault
        self.files = {}          # rel -> (mtime_ns, size, parsed)
        self.sig = None
        self.result = None       # data without note content
        self.notes = {}          # id -> parsed (with content)
        self.lock = threading.Lock()
        self.checked = 0.0       # last time the folder tree was walked
        self.stale = True

    def invalidate(self):
        """Call after writing through the server so the next build re-walks immediately."""
        self.stale = True

    def _walk(self):
        import os
        md, media, stack = [], [], [(str(self.vault), "")]
        while stack:
            path, rel = stack.pop()
            try:
                it = os.scandir(path)
            except OSError:
                continue
            with it:
                for e in it:
                    if e.name.startswith("."):
                        continue
                    r = f"{rel}/{e.name}" if rel else e.name
                    try:
                        if e.is_dir(follow_symlinks=False):
                            stack.append((e.path, r))
                            continue
                        if not e.is_file():
                            continue
                    except OSError:
                        continue
                    ext = e.name.rsplit(".", 1)[-1].lower() if "." in e.name else ""
                    if ext == "md":
                        st = e.stat()   # free on Windows (comes from the directory listing)
                        md.append((r, e.path, st.st_mtime_ns, st.st_size))
                    elif ext in MEDIA_EXT:
                        media.append(r)
        md.sort()
        return md, media

    def build(self):
        import time
        with self.lock:
            # external edits (e.g. in Obsidian) are picked up within MIN_RECHECK seconds; our own writes invalidate
            if self.result is not None and not self.stale and time.monotonic() - self.checked < self.MIN_RECHECK:
                return self.result
            md, media = self._walk()
            self.checked, self.stale = time.monotonic(), False
            sig = hash((tuple((r, m, s) for r, _, m, s in md), tuple(sorted(media))))
            if sig == self.sig and self.result is not None:
                return self.result
            files = {}
            for r, path, m, s in md:
                old = self.files.get(r)
                if old and old[0] == m and old[1] == s:
                    files[r] = old
                    continue
                try:
                    with open(path, encoding="utf-8", errors="replace") as f:
                        files[r] = (m, s, parse_note(r, path, m, f.read()))
                except OSError:
                    pass
            self.files = files
            self.notes = {p["id"]: p for _, _, p in files.values()}
            self.result = self._resolve(media)
            self.sig = sig
            return self.result

    def _resolve(self, media):
        parsed = [p for _, _, p in self.files.values()]
        lookup = {}
        for p in sorted(parsed, key=lambda p: (p["id"].count("/"), p["id"])):
            parts = p["id"].split("/")
            for i in range(len(parts)):
                lookup.setdefault("/".join(parts[i:]).lower(), p["id"])
        unresolved, notes = {}, []
        for p in sorted(parsed, key=lambda p: p["id"]):
            links, seen = [], set()

            def link(dest, missing):
                if dest:
                    if dest != p["id"] and dest not in seen:
                        seen.add(dest)
                        links.append(dest)
                elif missing:
                    src = unresolved.setdefault(missing, [])
                    if not src or src[-1] != p["id"]:
                        src.append(p["id"])

            for target in p["wiki"]:
                key = re.sub(r"\.md$", "", target, flags=re.I).lower()
                ext = key.rsplit(".", 1)[-1] if "." in key else ""
                link(lookup.get(key), None if ext in ASSET_EXT else target)
            for raw in p["mdl"]:
                ext_url = re.match(r"[a-z][a-z0-9+.-]*:", raw, re.I)
                link(None if ext_url else resolve_md(raw, p["id"], lookup),
                     None if ext_url else unquote(raw).rsplit("/", 1)[-1][:-3])
            notes.append({k: p[k] for k in ("id", "title", "folder", "mtime", "words", "tags", "props")} | {"links": links})
        assets = {}
        for rel in sorted(media, key=lambda r: (r.count("/"), r)):
            parts = rel.split("/")
            for i in range(len(parts)):
                assets.setdefault("/".join(parts[i:]).lower(), rel)
        return {
            "vault": self.vault.name or str(self.vault), "generated": datetime.now().isoformat(timespec="seconds"),
            "notes": notes, "lookup": lookup, "assets": assets,
            "unresolved": [{"target": t, "sources": s} for t, s in sorted(unresolved.items())],
        }

    def json(self):
        """Serialized result, cached until the vault changes."""
        res = self.build()
        if getattr(self, "_json_for", None) is not res:
            self._json, self._json_for = json.dumps(res, ensure_ascii=False), res
        return self._json

    def contents(self, ids):
        self.build()
        return {i: self.notes[i]["content"] for i in ids if i in self.notes}

    def search(self, q, limit=300):
        self.build()
        ql, out = q.lower().strip(), []
        if not ql:
            return out
        for nid, p in self.notes.items():
            i = p["lc"].find(ql)
            if i >= 0:
                c = p["content"]
                out.append({"id": nid, "snippet": re.sub(r"\s+", " ", c[max(0, i - 40):i + 80])})
                if len(out) >= limit:
                    break
        return out


def build(vault: Path, with_content: bool = True) -> dict:
    """One-shot build (static mode): note content is embedded."""
    idx = VaultIndex(vault)
    data = dict(idx.build())
    if with_content:
        data["notes"] = [n | {"content": idx.notes[n["id"]]["content"]} for n in data["notes"]]
    return data


BAD_CHARS = re.compile(r'[<>:"|?*\x00-\x1f\\]')


def note_path(vault: Path, nid: str) -> Path:
    """Map a note id ('Folder/Name') to a .md path inside the vault, or raise ValueError."""
    nid = nid.strip().strip("/")
    if nid.lower().endswith(".md"):
        nid = nid[:-3]
    segs = nid.split("/")
    if not nid or any(not s.strip() or s.startswith(".") or s.endswith(".") or BAD_CHARS.search(s) for s in segs):
        raise ValueError(f"invalid note name: {nid!r}")
    p = (vault / (nid + ".md")).resolve()
    if vault not in p.parents:
        raise ValueError("path escapes the vault")
    return p


def rewrite_links(vault: Path, old_id: str, new_id: str, lookup: dict) -> int:
    """After a rename, repoint [[old]] / [[folder/old]] wikilinks and [text](old.md) links.

    `lookup` must be the note lookup from *before* the rename (to resolve markdown links)."""
    import os
    old_base, new_base = old_id.rsplit("/", 1)[-1], new_id.rsplit("/", 1)[-1]
    pairs = [(old_id, new_id)] + ([(old_base, new_base)] if old_base != new_base else [])
    changed = 0
    for p in vault.rglob("*.md"):
        rel = p.relative_to(vault)
        if any(part.startswith(".") for part in rel.parts):
            continue
        # a renamed note keeps its own relative links pointing at the old location, so use old id for resolution
        me_old = old_id if rel.with_suffix("").as_posix() == new_id else rel.with_suffix("").as_posix()
        me_new = rel.with_suffix("").as_posix()
        text = p.read_text(encoding="utf-8", errors="replace")
        new = text
        for old, rep in pairs:
            new = re.sub(r"(\[\[)" + re.escape(old) + r"(?=[\]|#])", lambda m: m.group(1) + rep, new, flags=re.I)

        def fix(m):
            raw = m.group(1) or m.group(2)
            dest = resolve_md(raw, me_old, lookup)  # raw excludes any #fragment, which is left untouched
            if dest is None:
                return m.group(0)
            moved_dir = os.path.dirname(old_id) != os.path.dirname(new_id)
            if dest != old_id and not (me_new == new_id and moved_dir):
                return m.group(0)  # unrelated link: leave its style alone
            dest = new_id if dest == old_id else dest
            target = os.path.relpath(dest, os.path.dirname(me_new) or ".").replace("\\", "/") + ".md"
            if m.group(2) and " " in target:
                target = target.replace(" ", "%20")
            return m.group(0).replace(raw, target, 1) if unquote(target) != unquote(raw) else m.group(0)
        new = MDLINK.sub(fix, new)
        if new != text:
            p.write_text(new, encoding="utf-8")
            changed += 1
    return changed


def page(vault: Path, live: bool, index: "VaultIndex | None" = None) -> str:
    tpl = (Path(__file__).parent / "explorer_template.html").read_text(encoding="utf-8")
    raw = index.json() if index else json.dumps(build(vault), ensure_ascii=False)
    payload = raw.replace("</", "<\\/")
    return tpl.replace("/*__LIVE__*/false", "true" if live else "false").replace("/*__DATA__*/", payload)


def serve(vault: Path, port: int, open_browser: bool):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from urllib.parse import urlparse, parse_qs
    import os
    import time

    idx = VaultIndex(vault)
    t0 = time.time()
    n = len(idx.build()["notes"])
    print(f"indexed {n} notes in {time.time() - t0:.1f}s")
    allowed_hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def send(self, code, body, ctype="application/json"):
            b = body if isinstance(body, bytes) else (body if isinstance(body, str) and ctype != "application/json" else json.dumps(body)).encode("utf-8")
            self.send_response(code)
            textual = ctype.startswith("text/") or ctype == "application/json"
            self.send_header("Content-Type", ctype + ("; charset=utf-8" if textual else ""))
            if not textual:
                self.send_header("X-Content-Type-Options", "nosniff")
            if ctype == "image/svg+xml":  # an SVG opened directly must not run script in our origin
                self.send_header("Content-Security-Policy", "sandbox; default-src 'none'; style-src 'unsafe-inline'")
            self.send_header("Content-Length", str(len(b)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(b)

        def guard(self, write):
            if self.headers.get("Host") not in allowed_hosts:  # DNS-rebinding protection
                self.send(403, {"error": "bad host"})
                return False
            if write:
                origin = self.headers.get("Origin")
                if self.headers.get("X-Explorer") != "1" or (origin and urlparse(origin).netloc not in allowed_hosts):
                    self.send(403, {"error": "forbidden"})
                    return False
            return True

        def body(self):
            n = int(self.headers.get("Content-Length") or 0)
            return json.loads(self.rfile.read(n) or b"{}")

        def dispatch(self, fn, write):
            if not self.guard(write):
                return
            try:
                url = urlparse(self.path)
                fn(url.path, {k: v[0] for k, v in parse_qs(url.query).items()})
            except ValueError as e:
                self.send(400, {"error": str(e)})
            except FileNotFoundError:
                self.send(404, {"error": "not found"})
            except Exception as e:  # noqa: BLE001
                self.send(500, {"error": f"{type(e).__name__}: {e}"})

        def do_GET(self):
            def go(path, q):
                if path == "/":
                    self.send(200, page(vault, True, idx), "text/html")
                elif path == "/api/data":
                    self.send(200, idx.json().encode("utf-8"))
                elif path == "/api/search":
                    self.send(200, idx.search(q.get("q", "")))
                elif path.startswith("/vault/"):
                    rel = unquote(path[len("/vault/"):])
                    segs = rel.split("/")
                    f = (vault / rel).resolve()
                    if (any(not s or s.startswith(".") for s in segs) or f.suffix.lower()[1:] not in MEDIA_EXT
                            or vault not in f.parents or not f.is_file()):
                        raise FileNotFoundError
                    import mimetypes
                    self.send(200, f.read_bytes(), (mimetypes.guess_type(f.name)[0] or "application/octet-stream"))
                elif path == "/api/note":
                    p = note_path(vault, q.get("id", ""))
                    self.send(200, {"raw": p.read_text(encoding="utf-8", errors="replace"), "mtime": p.stat().st_mtime_ns})
                else:
                    self.send(404, {"error": "not found"})
            self.dispatch(go, False)

        def do_PUT(self):  # save existing
            def go(path, q):
                d = self.body()
                p = note_path(vault, d.get("id", ""))
                if not p.is_file():
                    raise FileNotFoundError
                if d.get("mtime") and p.stat().st_mtime_ns != d["mtime"]:
                    self.send(409, {"error": "file changed on disk since you opened it"})
                    return
                p.write_bytes(d.get("raw", "").encode("utf-8"))
                idx.invalidate()
                self.send(200, {"ok": True, "mtime": p.stat().st_mtime_ns})
            self.dispatch(go, True)

        def do_POST(self):  # create / rename
            def go(path, q):
                d = self.body()
                if path == "/api/contents":  # read-only; POST only to carry a long id list
                    ids = d.get("ids", [])
                    if not isinstance(ids, list) or len(ids) > 2000:
                        raise ValueError("bad ids")
                    self.send(200, idx.contents([str(i) for i in ids]))
                elif path == "/api/note":
                    p = note_path(vault, d.get("id", ""))
                    if p.exists():
                        raise ValueError("a note with that name already exists")
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_bytes(d.get("raw", "").encode("utf-8"))
                    idx.invalidate()
                    self.send(200, {"ok": True, "mtime": p.stat().st_mtime_ns})
                elif path == "/api/rename":
                    src, dst = note_path(vault, d.get("id", "")), note_path(vault, d.get("new", ""))
                    if not src.is_file():
                        raise FileNotFoundError
                    if dst.exists() and dst != src:
                        raise ValueError("target already exists")
                    old_lookup = idx.build()["lookup"]
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(src, dst)
                    n = rewrite_links(vault, d["id"].strip("/").removesuffix(".md"), d["new"].strip("/").removesuffix(".md"), old_lookup)
                    idx.invalidate()
                    self.send(200, {"ok": True, "updated": n})
                else:
                    self.send(404, {"error": "not found"})
            self.dispatch(go, True)

        def do_DELETE(self):  # move to <vault>/.trash (recoverable)
            def go(path, q):
                p = note_path(vault, q.get("id", ""))
                if not p.is_file():
                    raise FileNotFoundError
                trash = vault / ".trash"
                trash.mkdir(exist_ok=True)
                dst = trash / p.name
                if dst.exists():
                    dst = trash / f"{p.stem}-{int(time.time())}{p.suffix}"
                shutil.move(str(p), str(dst))
                idx.invalidate()
                self.send(200, {"ok": True, "trashed": dst.name})
            self.dispatch(go, True)

    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    url = f"http://127.0.0.1:{port}/"
    print(f"Serving {vault} at {url}  (Ctrl+C to stop; deleted notes go to .trash)")
    if open_browser:
        webbrowser.open(url)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--serve", action="store_true", help="run local server with note editing (create/edit/rename/delete)")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("vault", nargs="?", default=".", help="vault directory (default: .)")
    ap.add_argument("-o", "--out", default="explorer.html")
    ap.add_argument("--open", action="store_true", help="open in the default browser")
    a = ap.parse_args()

    vault = Path(a.vault).expanduser().resolve()
    if not vault.is_dir():
        sys.exit(f"not a directory: {vault}")
    if a.serve:
        return serve(vault, a.port, a.open)
    out = Path(a.out).resolve()
    out.write_text(page(vault, False), encoding="utf-8")
    print(f"static read-only explorer -> {out}")
    if a.open:
        webbrowser.open(out.as_uri())


if __name__ == "__main__":
    main()
