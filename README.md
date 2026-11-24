# fastobsidian

A quick, dependency-free explorer and editor for an [Obsidian](https://obsidian.md) vault.
One Python script (standard library only) plus one HTML template. No build step, no network calls.

## Features

- **Index** – stats, tag cloud, most connected / recently modified notes, orphans, unresolved links, sortable A–Z table
- **Graph** – force-directed canvas: pan, zoom, drag, hover highlight, click to open, focus on a note (1–3 hops), colour by cluster or folder, filter by tag, spread slider, unresolved links as ghost nodes
- **Search** – folder tree plus full-text search with snippets; click a tag to filter
- **Notes** – rendered markdown (headings, lists, tasks, tables, code, callouts, highlights, tags), backlinks and outgoing links
- **Navigation** – both `[[wikilinks]]` (with aliases and `#headings`) and standard `[text](path/note.md)` links, relative or vault-root
- **Table of contents** – clickable per-note TOC that tracks your scroll position
- **Theme** – glass-style UI with a light/dark toggle (follows the system by default, choice is remembered)
- **Large vaults** (server mode) – incremental index that re-reads only changed files; search runs server-side
- **Editing** (server mode) – create, edit, rename and delete notes with a split markdown editor and live preview

## Usage

Requires Python 3.9+.

```bash
# Live mode: browse and edit at http://127.0.0.1:8765/
python obsidian_explorer.py /path/to/vault --serve --open

# Read-only: write a single self-contained explorer.html
python obsidian_explorer.py /path/to/vault -o explorer.html --open
```

Options: `--port N` (default 8765), `-o FILE` (static output), `--open` (launch browser).
Try it with the bundled `sample-vault/`.

## MCP server (for AI agents)

`python obsidian_explorer.py /path/to/vault --mcp` speaks MCP over stdio. Tools: `search` (ranked: name > alias > tag > body, optional `tag`),
`read`, `write` (`append` default, `overwrite`, `create`; `tags`/`aliases` become frontmatter on new notes), `index`, `backlinks`, `rename`, `delete` (to `.trash`).

```json
{ "mcpServers": { "vault": { "command": "python", "args": ["/path/obsidian_explorer.py", "/path/to/vault", "--mcp"] } } }
```

Frontmatter `aliases` also resolve `[[alias]]` links. Run tests with `python -m unittest discover -s tests`.

## Editing

In live mode each note has **edit**, **rename** and **delete** buttons, and the sidebar has **+ New note**
(path like `Folder/Name`). Ctrl+S saves; a toolbar inserts bold, italic, headings, lists, tasks, `[[links]]`, code and tags.

- **Delete is recoverable**: files move to `<vault>/.trash/`.
- **Rename updates links**: `[[wikilinks]]` in other notes are repointed. Markdown `[text](x.md)` links are not rewritten.
- **Conflict check**: saving is refused if the file changed on disk since you opened it.
- Saving normalises line endings to LF.

## Security

The server binds to `127.0.0.1` only, rejects unexpected `Host`/`Origin` headers, requires a custom header on writes,
and only touches `.md` files inside the vault (paths with `..` or dot-folders are refused).
It has no authentication, so do not expose it on a network.

## Notes

- Folders starting with `.` (e.g. `.obsidian`, `.trash`) are ignored.
- Attachments (images, PDF, audio, video) are detected and resolved by the index; the file type list is `MEDIA_EXT` in `obsidian_explorer.py`.
- `explorer.html` embeds your vault's content; it is gitignored by default, so do not commit or share it unintentionally.

## Files

| File | Purpose |
|---|---|
| `obsidian_explorer.py` | Vault parser, static generator and local server |
| `explorer_template.html` | UI (index, graph, note view, editor) |
| `sample-vault/` | Demo notes |
