---
name: lumen-cli
description: "Use when the user wants to work with the lumen CLI to inspect diff review state (status/current-hunk/current-file/full-context), list/add/update/delete annotations, manage hunk tags, or check API connectivity. Trigger on requests like 'use lumen-cli', 'status', 'current hunk', 'list annotations', 'add/update/delete annotation', 'tag hunk', or 'ping'."
---

# Lumen CLI

Use `lumen-cli` commands only.

## Commands

- `lumen-cli status` — show scope, cwd, current file, focused hunk, and counts.
- `lumen-cli current-hunk` — return the focused hunk context + diff + annotation (if any).
- `lumen-cli hunks [--current]` — list hunks (all files or current file).
- `lumen-cli hunk <file_index> <hunk_index>` — fetch a specific hunk by index.
- `lumen-cli current-file [--new-only|--old-only]` — return full file contents (or just new/old).
- `lumen-cli annotations [--current]` — list annotations (all or current file only).
- `lumen-cli annotate <text> [--file-index N --hunk-index M]` — create an annotation on the focused or targeted hunk.
- `lumen-cli update <id> <text>` — update annotation content by id.
- `lumen-cli delete <id>` — delete annotation by id.
- `lumen-cli tag list` — list available tags in the repo.
- `lumen-cli tag current` — show tags for the focused hunk.
- `lumen-cli tag set <tags...> [--file-index N --hunk-index M]` — replace tags for the focused or targeted hunk.
- `lumen-cli tag add <tag> [--file-index N --hunk-index M]` — add a tag to the focused or targeted hunk.
- `lumen-cli tag remove <tag> [--file-index N --hunk-index M]` — remove a tag from the focused or targeted hunk.
- `lumen-cli tag clear [--file-index N --hunk-index M]` — clear all tags from the focused or targeted hunk.
- `lumen-cli tags [--current]` — list tags per hunk (all files or current file only).
- `lumen-cli full-context` — fetch status + current hunk + annotations in one call.
- `lumen-cli ping` — check API connectivity (nonzero exit on failure).

## Usage Guidance

- Use `lumen-cli status` to get `cwd`; file paths returned by other commands are relative to it.
- Use `lumen-cli hunks` to discover `file_index` and `hunk_index` values for targeted operations.
- Treat annotations as collaborative threads: lines without a prefix are user notes, lines starting with `Agent: ` are agent responses.
- When responding to an annotation, append two newlines and then a line starting with `Agent: `; keep the existing content intact unless explicitly asked to replace it.
- Surface errors exactly and suggest retrying once the API is reachable.

## Examples

```bash
# list hunks (all files)
lumen-cli hunks

# fetch details for a specific hunk
lumen-cli hunk 2 5

# annotate a specific hunk
lumen-cli annotate "TICKET-123: please add validation" --file-index 2 --hunk-index 5

# tag a specific hunk
lumen-cli tag add TICKET-123 --file-index 2 --hunk-index 5

# list hunks that have tags
lumen-cli tags
```
