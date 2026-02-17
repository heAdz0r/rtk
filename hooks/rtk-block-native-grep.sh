#!/bin/bash
# RTK block hook for Claude Code PreToolUse:Grep
# Blocks the native Grep tool and instructs Claude to use rtk grep via Bash instead.
# This ensures all search operations go through rtk for token savings tracking.

# Output deny decision with guidance (canonical schema per Context7 docs)
cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Grep tool is disabled. Use `rtk rgai <query>` (semantic/fuzzy, preferred) or `rtk grep <pattern> [path]` (exact/regex) via Bash tool instead."
  }
}
EOF
