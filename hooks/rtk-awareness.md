# RTK - Rust Token Killer

**Usage**: Token-optimized CLI proxy (60-90% savings on dev operations)

## Meta Commands (always use rtk directly)

```bash
rtk gain              # Show token savings analytics
rtk gain --history    # Show command usage history with savings
rtk discover          # Analyze Claude Code history for missed opportunities
rtk proxy <cmd>       # Execute raw command without filtering (for debugging)
```

## Installation Verification

```bash
rtk --version         # Should show: rtk X.Y.Z
rtk gain              # Should work (not "command not found")
which rtk             # Verify correct binary
```

⚠️ **Name collision**: If `rtk gain` fails, you may have reachingforthejack/rtk (Rust Type Kit) installed instead.

## Hook-Based Usage

All other commands are automatically rewritten by the Claude Code hook.
Example: `git status` → `rtk git status` (transparent, 0 tokens overhead)
Example: `grepai search "auth token refresh"` → `rtk rgai "auth token refresh"`
Example: `bun run build` → `rtk bun run build`

## JS/TS Defaults (compact first)

```bash
rtk bun run <script>      # concise default (tests/warnings/errors in a few lines)
rtk bun run <script> -v   # verbose details when needed
rtk npm run <script>      # concise script output
rtk npx <tool> ...        # routes to specialized filters when available
```

## Search Priority Policy (MANDATORY)

Search priority (mandatory): rgai > rg > grep.

- `rtk rgai` — semantic/intention-based discovery (first choice)
- `rtk grep` — exact/regex matching (second choice, internal rg -> grep fallback)
- Native Grep/Read tools may be blocked by hook; use `rtk grep` / `rtk read` via Bash

## Semantic Search

```bash
rtk rgai "auth token refresh"         # Intent-aware code search
rtk rgai auth token refresh --compact # Unquoted multi-word query
rtk rgai "auth token refresh" --json  # Machine-readable output
```

## Precise File Reads

```bash
rtk read src/main.rs --level none --from 200 --to 320
```

## Safe File Writes

Use `rtk write` for deterministic, atomic edits (idempotent + durable by default).
Native Edit/Write tools are blocked by hook — always use `rtk write` via Bash.

### Single operations

```bash
rtk write replace path/to/file --from old --to new [--all] [--dry-run]
rtk write patch path/to/file --old "block A" --new "block B" [--all] [--dry-run]
rtk write set path/to/config.json --key a.b --value 42 --value-type number
rtk write set path/to/config.toml --key a.b --value true --format toml
```

### Batch mode (multi-file, single process)

```bash
rtk write batch --plan '[
  {"op":"replace","file":"src/lib.rs","from":"old_fn","to":"new_fn"},
  {"op":"patch","file":"src/main.rs","old":"block A","new":"block B"},
  {"op":"set","file":"config.json","key":"version","value":"2.0"}
]' [--dry-run]
```

Batch groups I/O, reports `applied/failed/total`, and continues on partial failure.

### Key properties

- **Atomic**: tempfile + rename (no partial writes)
- **Idempotent**: noop if content already matches (replace, patch, set)
- **--dry-run**: preview without writing
- **Output modes**: `--output quiet|concise|json`

Prefer this over ad-hoc `sed -i` / `perl -pi` when the transformation fits these primitives.

## Tabular Files (CSV/TSV)

- `rtk read <file>` in filtered modes returns a compact digest (rows/cols/sample/sampled-stats).
- Use `--level none --from/--to` for exact row content.

## Read Cache

- Filtered `rtk read` output is cached for repeat reads.
- Cache auto-invalidates when file path/size/mtime change.

Refer to CLAUDE.md for full command reference.
