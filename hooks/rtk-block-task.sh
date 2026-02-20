#!/bin/bash
# RTK hook: block ALL Task/subagent calls.
# Subagents waste tokens. Use rtk rgai/grep/read directly.
# Override: RTK_ALLOW_SUBAGENTS=1

if [ "${RTK_ALLOW_SUBAGENTS:-0}" = "1" ]; then
  exit 0
fi

cat <<'EOF_JSON'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Subagents disabled by RTK policy. Use direct tools instead: rtk rgai (semantic search), rtk grep (regex), rtk read (file read), rtk write (file write). Override: RTK_ALLOW_SUBAGENTS=1"
  }
}
EOF_JSON
