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

### Non-goals
- No streaming/watch mode.
- No AI query/ask command.
