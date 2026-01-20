---
name: lumen-cli
description: "Use when the user wants to work with the lumen CLI to inspect diff review state (status/current-hunk/current-file/full-context), list/add/update/delete annotations, manage hunk tags, or check API connectivity. Trigger on requests like 'use lumen-cli', 'status', 'current hunk', 'list annotations', 'add/update/delete annotation', 'tag hunk', or 'ping'."
---

# Lumen CLI

Use `lumen-cli` commands only.

## Commands

- `lumen-cli status` — show scope, cwd, current file, focused hunk, and counts.
- `lumen-cli current-hunk` — return the focused hunk context + diff + annotation (if any).
- `lumen-cli current-file [--new-only|--old-only]` — return full file contents (or just new/old).
- `lumen-cli annotations [--current]` — list annotations (all or current file only).
- `lumen-cli annotate <text>` — create an annotation on the focused hunk.
- `lumen-cli update <id> <text>` — update annotation content by id.
- `lumen-cli delete <id>` — delete annotation by id.
- `lumen-cli tag list` — list available tags in the repo.
- `lumen-cli tag current` — show tags for the focused hunk.
- `lumen-cli tag set <tags...>` — replace tags for the focused hunk.
- `lumen-cli tag add <tag>` — add a tag to the focused hunk.
- `lumen-cli tag remove <tag>` — remove a tag from the focused hunk.
- `lumen-cli tag clear` — clear all tags from the focused hunk.
- `lumen-cli full-context` — fetch status + current hunk + annotations in one call.
- `lumen-cli ping` — check API connectivity (nonzero exit on failure).

## Usage Guidance

- Use `lumen-cli status` to get `cwd`; file paths returned by other commands are relative to it.
- Treat annotations as collaborative threads: lines without a prefix are user notes, lines starting with `Agent: ` are agent responses.
- When responding to an annotation, append two newlines and then a line starting with `Agent: `; keep the existing content intact unless explicitly asked to replace it.
- Surface errors exactly and suggest retrying once the API is reachable.
