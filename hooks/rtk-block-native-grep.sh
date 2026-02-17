#!/bin/bash
# RTK block hook for Claude Code PreToolUse:Grep
# Blocks native Grep tool and instructs Claude to use rtk grep/rgai via Bash instead.
# This ensures search operations go through RTK for filtering and token savings.

# Output deny decision with guidance (canonical schema per Context7 docs)
cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Grep tool is disabled. Use `rtk rgai <query>` (semantic/fuzzy) or `rtk grep <pattern> [path]` (exact/regex) via Bash instead."
  }
}
EOF
