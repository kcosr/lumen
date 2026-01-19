---
name: lumen-cli
description: "Use when the user wants to work with the lumen CLI to query or manage annotations, or to fetch status/current-hunk/current-file/full-context/ping via command line. Trigger on requests like 'use lumen-cli', 'list annotations', 'create/update/delete annotation', 'current hunk', or 'status'."
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
- `lumen-cli full-context` — fetch status + current hunk + annotations in one call.
- `lumen-cli ping` — check API connectivity (nonzero exit on failure).

## Usage Guidance

- Use `lumen-cli status` to get `cwd`; file paths returned by other commands are relative to it.
- Use `--format json` or `--format json-pretty` when machine-readable output is needed.
- Surface errors exactly and suggest retrying once the API is reachable.
