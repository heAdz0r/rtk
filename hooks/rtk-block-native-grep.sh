#!/bin/bash
# RTK block hook for Claude Code PreToolUse:Grep/Read
# Blocks native Grep and Read tools and instructs Claude to use rtk via Bash instead.
# This ensures search/read operations go through RTK for filtering and token savings.

# Output deny decision with guidance (canonical schema per Context7 docs)
cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Grep/Read tools are disabled. Use `rtk rgai <query>` (semantic/fuzzy), `rtk grep <pattern> [path]` (exact/regex), or `rtk read <file> [--level none] [--from N --to M]` via Bash instead."
  }
}
EOF
