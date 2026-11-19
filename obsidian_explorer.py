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
ASSET_EXT = {"png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "pdf", "mp3", "mp4",
             "wav", "ogg", "webm", "mov", "canvas", "base"}


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


def build(vault: Path):
    files = sorted(
        p for p in vault.rglob("*.md")
        if not any(part.startswith(".") for part in p.relative_to(vault).parts)
    )
    notes = []
    for p in files:
        rel = p.relative_to(vault).with_suffix("").as_posix()
        text = p.read_text(encoding="utf-8", errors="replace")
        fm, body = parse_frontmatter(text)
        notes.append({
            "id": rel, "title": p.stem, "folder": rel.rsplit("/", 1)[0] if "/" in rel else "",
            "fm": fm, "content": body.strip(), "mtime": int(p.stat().st_mtime),
            "words": len(re.findall(r"\w+", body)),
        })

    # lookup: lowercase name / any path suffix -> id (first wins; shorter paths sorted first)
    lookup = {}
    for n in sorted(notes, key=lambda n: n["id"].count("/")):
        parts = n["id"].split("/")
        for i in range(len(parts)):
            lookup.setdefault("/".join(parts[i:]).lower(), n["id"])

    unresolved = {}
    for n in notes:
        stripped = INLINE_CODE.sub("", FENCE.sub("", n["content"]))
        tags = set()
        for t in n["fm"].get("tags", []) + n["fm"].get("tag", []) if isinstance(n["fm"].get("tags", []), list) else []:
            tags.add(t.lstrip("#"))
        tags |= set(TAG.findall(stripped))
        links = []
        for _, target, _ in WIKI.findall(stripped):
            target = target.strip()
            if not target:
                continue
            key = re.sub(r"\.md$", "", target, flags=re.I).lower()
            ext = key.rsplit(".", 1)[-1] if "." in key else ""
            dest = lookup.get(key)
            if dest:
                if dest != n["id"] and dest not in links:
                    links.append(dest)
            elif ext not in ASSET_EXT:
                src = unresolved.setdefault(target, [])
                if n["id"] not in src:
                    src.append(n["id"])
        # standard markdown links to .md files: [text](path/note.md#heading)
        for m in MDLINK.finditer(stripped):
            raw = m.group(1) or m.group(2)
            dest = resolve_md(raw, n["id"], lookup)
            if dest:
                if dest != n["id"] and dest not in links:
                    links.append(dest)
            elif not re.match(r"[a-z][a-z0-9+.-]*:", raw, re.I):
                src = unresolved.setdefault(unquote(raw).rsplit("/", 1)[-1][:-3], [])
                if n["id"] not in src:
                    src.append(n["id"])
        n["tags"] = sorted(tags)
        n["links"] = links
        del n["fm"]

    return {
        "vault": vault.name or str(vault), "generated": datetime.now().isoformat(timespec="seconds"),
        "notes": notes, "lookup": lookup,
        "unresolved": [{"target": t, "sources": s} for t, s in sorted(unresolved.items())],
    }


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


def rewrite_links(vault: Path, old_id: str, new_id: str) -> int:
    """After a rename, repoint [[old]] / [[folder/old]] wikilinks in all notes."""
    old_base, new_base = old_id.rsplit("/", 1)[-1], new_id.rsplit("/", 1)[-1]
    pairs = [(old_id, new_id)] + ([(old_base, new_base)] if old_base != new_base else [])
    changed = 0
    for p in vault.rglob("*.md"):
        if any(part.startswith(".") for part in p.relative_to(vault).parts):
            continue
        text = p.read_text(encoding="utf-8", errors="replace")
        new = text
        for old, rep in pairs:
            new = re.sub(r"(\[\[)" + re.escape(old) + r"(?=[\]|#])", lambda m: m.group(1) + rep, new, flags=re.I)
        if new != text:
            p.write_text(new, encoding="utf-8")
            changed += 1
    return changed


def page(vault: Path, live: bool) -> str:
    tpl = (Path(__file__).parent / "explorer_template.html").read_text(encoding="utf-8")
    payload = json.dumps(build(vault), ensure_ascii=False).replace("</", "<\\/")
    return tpl.replace("/*__LIVE__*/false", "true" if live else "false").replace("/*__DATA__*/", payload)


def serve(vault: Path, port: int, open_browser: bool):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from urllib.parse import urlparse, parse_qs
    import os
    import time

    allowed_hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def send(self, code, body, ctype="application/json"):
            b = body if isinstance(body, bytes) else (body if isinstance(body, str) and ctype != "application/json" else json.dumps(body)).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", ctype + "; charset=utf-8")
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
                    self.send(200, page(vault, True), "text/html")
                elif path == "/api/data":
                    self.send(200, build(vault))
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
                self.send(200, {"ok": True, "mtime": p.stat().st_mtime_ns})
            self.dispatch(go, True)

        def do_POST(self):  # create / rename
            def go(path, q):
                d = self.body()
                if path == "/api/note":
                    p = note_path(vault, d.get("id", ""))
                    if p.exists():
                        raise ValueError("a note with that name already exists")
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_bytes(d.get("raw", "").encode("utf-8"))
                    self.send(200, {"ok": True, "mtime": p.stat().st_mtime_ns})
                elif path == "/api/rename":
                    src, dst = note_path(vault, d.get("id", "")), note_path(vault, d.get("new", ""))
                    if not src.is_file():
                        raise FileNotFoundError
                    if dst.exists() and dst != src:
                        raise ValueError("target already exists")
                    dst.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(src, dst)
                    n = rewrite_links(vault, d["id"].strip("/").removesuffix(".md"), d["new"].strip("/").removesuffix(".md"))
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
