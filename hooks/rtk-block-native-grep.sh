#!/bin/bash
# RTK hook for Claude Code PreToolUse:Grep
# Default behavior: allow native Grep, but emit guidance to prefer RTK search.
# Set RTK_BLOCK_NATIVE_GREP=1 to enforce strict blocking.
# Set RTK_NOTIFY_NATIVE_GREP=0 to suppress allow-mode notifications.

if [ "${RTK_BLOCK_NATIVE_GREP:-0}" != "1" ]; then
  if [ "${RTK_NOTIFY_NATIVE_GREP:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Grep used. Prefer `rtk rgai <query>` (semantic/fuzzy) or `rtk grep <pattern> [path]` (exact/regex) via Bash for compact RTK output."
  }
}
EOF
  exit 0
fi

cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Grep tool is disabled (RTK_BLOCK_NATIVE_GREP=1). Use `rtk rgai <query>` (semantic/fuzzy) or `rtk grep <pattern> [path]` (exact/regex) via Bash instead."
  }
}
EOF
