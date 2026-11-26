# fastobsidian

A quick, dependency-free explorer and editor for an [Obsidian](https://obsidian.md) vault.
One Python script (standard library only) plus one HTML template. No build step, no network calls.

## Features

- **Index** – stats, tag cloud, most connected / recently modified notes, orphans, unresolved links, sortable A–Z table
- **Graph** – force-directed canvas: pan, zoom, drag, hover highlight, click to open, focus on a note (1–3 hops), colour by cluster or folder, filter by tag, spread slider, unresolved links as ghost nodes
- **Search** – folder tree plus full-text search with snippets, ranked by name, alias, tag, then body; click a tag to filter
- **Notes** – rendered markdown (headings, lists, tasks, tables, code, callouts, highlights, tags), backlinks and outgoing links
- **Navigation** – both `[[wikilinks]]` (with `#headings`, `|display` text, and frontmatter `aliases` as link targets) and standard `[text](path/note.md)` links, relative or vault-root
- **Focus mode** – Ctrl+F on a note hides the columns and goes full screen; press again (or Esc) to restore them
- **Table of contents** – clickable per-note TOC that tracks your scroll position
- **Theme** – glass-style UI with a light/dark toggle (follows the system by default, choice is remembered);
  `--theme` applies the vault's own Obsidian theme and enabled CSS snippets instead
- **Large vaults** (server mode) – incremental index that re-reads only changed files; search runs server-side
- **Live refresh** (server mode) – notes edited outside the app (e.g. in Obsidian) update in the open page within about a second
- **Editing** (server mode) – create, edit, rename and delete notes with a split markdown editor and live preview
- **Vault switching** (server mode) – Ctrl+O opens another vault without restarting
- **MCP server** – `--mcp` exposes search, read, write/append, index, backlinks, rename and delete to AI agents over stdio

## Usage

Requires Python 3.9+.

```bash
# Live mode: browse and edit at http://127.0.0.1:8765/
python obsidian_explorer.py /path/to/vault --serve --open

# Read-only: write a single self-contained explorer.html
python obsidian_explorer.py /path/to/vault -o explorer.html --open
```

Options: `--port N` (default 8765), `-o FILE` (static output), `--open` (launch browser), `--mcp` (MCP server on stdio, see below),
`--theme` (use the vault's Obsidian theme, see below), `--watch SECONDS` (how often the server checks the vault for outside changes
while a page is open, default 1; `0` turns it off).
Try it with the bundled `sample-vault/`.

## Obsidian themes and snippets

With `--theme`, the theme and CSS snippets enabled in `<vault>/.obsidian/appearance.json` are added to the page
(`.obsidian/themes/<Name>/theme.css` and `.obsidian/snippets/<name>.css`), and the explorer's colours follow the theme's
Obsidian variables (`--background-primary`, `--text-normal`, `--interactive-accent`, …). The light/dark toggle still works,
and its default comes from Obsidian's base theme. Only colours are mapped: a theme's layout rules target Obsidian's own
interface, so they mostly have no effect here. A theme can load fonts or images from the internet, so with `--theme` the
page may make network requests. It is off by default.

## Live refresh

In server mode, an open page listens on `/api/events` and the server checks the vault every `--watch` seconds while a page is
open (no checks when none is). When something changes on disk, the index, sidebar and graph reload and the open note is
re-rendered at the same scroll position. In the editor, a note without local changes takes the new text; with unsaved
changes, your text is kept and a warning says the file changed on disk (saving would be refused anyway).

## MCP server (for AI agents)

`python obsidian_explorer.py /path/to/vault --mcp` speaks MCP over stdio. Tools: `search` (ranked: name > alias > tag > body, optional `tag`),
`read`, `write` (`append` default, `overwrite`, `create`; `tags`/`aliases` become frontmatter on new notes), `index`, `backlinks`, `rename`, `delete` (to `.trash`).

```json
{ "mcpServers": { "vault": { "command": "python", "args": ["/path/obsidian_explorer.py", "/path/to/vault", "--mcp"] } } }
```

Frontmatter `aliases` also resolve `[[alias]]` links. Run tests with `python -m unittest discover -s tests`.

## Editing

In live mode each note has **edit**, **rename** and **delete** buttons, and the sidebar has **+ New note**
(path like `Folder/Name`). Ctrl+S saves; Ctrl+O switches to another vault (asks for a folder path); a toolbar inserts bold, italic, headings, lists, tasks, `[[links]]`, code and tags.

- **Delete is recoverable**: files move to `<vault>/.trash/`.
- **Rename updates links**: `[[wikilinks]]` and markdown `[text](x.md)` links in other notes are repointed
  (markdown links are rewritten relative to each note; a moved note's own relative links are fixed too).
- **Conflict check**: saving is refused if the file changed on disk since you opened it.
- Saving normalises line endings to LF.

## Security

The server binds to `127.0.0.1` only, rejects unexpected `Host`/`Origin` headers, requires a custom header on writes,
and only touches `.md` files inside the vault (paths with `..` or dot-folders are refused).
It has no authentication, so do not expose it on a network.

Symlinks and Windows junctions are never followed: linked files and folders are left out of the index, and reads,
writes, renames and attachment requests through a link are refused, so a link inside the vault cannot redirect a write
to a file elsewhere. (The check runs just before each file operation; it is not atomic.) Notes over 32 MiB are left out
of the index and refused by read and write (`MAX_NOTE_BYTES`).

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
| `tests/` | Unit tests (`python -m unittest discover -s tests`), run by GitHub Actions on Linux, Windows and macOS |

## License

MIT, see [LICENSE](LICENSE).
