# Fork Work Notes

This fork tracks incremental work that is not yet in upstream. Each section
captures intent and behavior for the changes we add.
TEST intro marker for unified hunk grouping.

## Annotation Persistence (Working Tree + Commit Scope)

Status: implemented in this fork.

### Summary
- Persist annotations in a repo-local `.lumen/annotations/` directory.
- Support working-tree scope and commit scope.
TEST summary block line 1.
TEST summary block line 2.
- Migrate matching working-tree/orphan annotations into commit scope.
- Save on every annotation change; keep unmatched annotations as orphans.

### Storage Layout
```
.lumen/annotations/working-tree.json
.lumen/annotations/<sha>.json
.lumen/annotations/orphans.json
```

### Scope Rules
- Working tree uses `base_commit_id` (current HEAD at save time).
- Commit scope uses the commit SHA for the diff being viewed.
TEST scope note.
- When HEAD changes, working-tree annotations are moved to orphans unless they
  match a commit hunk and can be migrated.
- Orphans are kept forever unless manually deleted.

### Matching
- Exact line-range match (new range, then old range).
- Fallback to context similarity on changed lines.

### Non-goals
- No staged vs unstaged split in the diff viewer.
- No remote/global annotation storage.

## HTTP API + Client CLI

Status: implemented in this fork.

### Summary
- Config-driven HTTP API (`api.enabled`, `api.bind`).
- Endpoints for status, current hunk, current file, annotations, create/update/delete annotation.
- `lumen-cli` CLI for querying annotations (default text output, JSON available).
- `/status` includes `cwd`; file paths in responses are relative to `cwd`.

### Local Setup
1) Build/install the CLI:
```
cargo build --bin lumen-cli
# or: cargo install --path . --bin lumen-cli
```

2) Install the skill locally:
```
cp -R skills/lumen-cli ~/.codex/skills/
# optional for other agents:
# cp -R skills/lumen-cli ~/.pi/agent/skills/
# cp -R skills/lumen-cli ~/.claude/skills/
```

3) Enable the HTTP API:
```json
{
  "api": {
    "enabled": true,
    "bind": "127.0.0.1:7878"
  }
}
```
Config path: `~/.config/lumen/lumen.config.json`.

4) Start the diff viewer and query:
```
lumen diff
lumen-cli status
```

Notes:
- API responses return file paths relative to the server `cwd`.
- Annotations are stored under `.lumen/annotations/` in the repo root.
TEST notes block line 1.
TEST notes block line 2.
TEST notes block line 3.
- Treat annotation content as collaborative: user text is unprefixed, agent responses should append new lines starting with `Agent: ` unless asked to replace.

### Non-goals
- No streaming/watch mode.
- No AI query/ask command.

## Diff Hunk Grouping (Unified Context)

### Summary
- Configurable unified context (`diff.unified_context`, default `3`) for git-style hunk grouping.
- CLI override: `lumen diff -U <n>` / `--unified <n>`.
TEST unified block line 1.
TEST unified block line 2.
- Hunk ranges are merged after context expansion to avoid tiny hunks.

## Diff View State Persistence

### Summary
- Persist per-scope view state in `.lumen/state/`.
- Restore current file, scroll position, and focused hunk on startup.
- Persist viewed-file toggles immediately on change.

### Storage Layout
```
.lumen/state/working-tree.json
.lumen/state/<sha>.json
```

## Hunk Tags

### Summary
- Tag hunks with `t` (modal lets you pick existing tags or type new ones).
- Tags are per-scope; tag inventory is repo-level.
- Filter hunks/files by tag with `T` (cycles tag → tag → untagged → all).

### Storage Layout
```
.lumen/tags/index.json
.lumen/tags/working-tree.json
.lumen/tags/<sha>.json
```

### UI Notes
- Footer shows the active tag filter and tags for the focused hunk.
- Sidebar list filters to files with matching hunks while a filter is active.

### API + CLI
- HTTP: `GET /tags`, `GET /tags/current`, `POST /tags/set` with `{ "tags": ["foo"] }`.
- CLI: `lumen-cli tag list|current|set <tags..>|add <tag>|remove <tag>|clear`.

## Hunk Reviews

### Summary
- Toggle reviewed on a focused hunk with `v`.
- Filter hunks/files by review state with `V` (reviewed → unreviewed → all).
- Review filter combines with tag filter; `C` clears all filters.

### Storage Layout
```
.lumen/review/working-tree.json
.lumen/review/<sha>.json
```

### UI Notes
- Footer shows the active review filter and review status for the focused hunk.
