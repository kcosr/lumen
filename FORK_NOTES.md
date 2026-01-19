# Fork Work Notes

This fork tracks incremental work that is not yet in upstream. Each section
captures intent and behavior for the changes we add.

## Annotation Persistence (Working Tree + Commit Scope)

Status: implemented in this fork.

### Summary
- Persist annotations in a repo-local `.lumen/annotations/` directory.
- Support working-tree scope and commit scope.
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
- Treat annotation content as collaborative: user text is unprefixed, agent responses should append new lines starting with `Agent: ` unless asked to replace.

### Non-goals
- No streaming/watch mode.
- No AI query/ask command.
