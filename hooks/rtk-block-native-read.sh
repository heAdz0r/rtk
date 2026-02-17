#!/bin/bash
# RTK block hook for Claude Code PreToolUse:Read
# Blocks native Read tool and instructs Claude to use rtk read via Bash instead.
# This ensures read operations go through RTK for filtering, caching, and token savings.

# Output deny decision with guidance (canonical schema per Context7 docs)
cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Read tool is disabled. Use `rtk read <file> [--level none] [--from N --to M]` via Bash instead. Filtered reads are cached; use --level none for exact content."
  }
}
EOF
