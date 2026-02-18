#!/bin/bash
# RTK hook for Claude Code PreToolUse:Grep
# Default behavior: hard deny native Grep and route to RTK search.
# Set RTK_ALLOW_NATIVE_GREP=1 (or legacy RTK_BLOCK_NATIVE_GREP=0) to allow native Grep.
# Set RTK_NOTIFY_NATIVE_GREP=0 to suppress allow-mode notifications.

# Explicit allow override (policy default is deny)
if [ "${RTK_ALLOW_NATIVE_GREP:-0}" = "1" ] || [ "${RTK_BLOCK_NATIVE_GREP:-1}" = "0" ]; then
  if [ "${RTK_NOTIFY_NATIVE_GREP:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Grep explicitly allowed by policy override. Prefer `rtk rgai <query>` (semantic/fuzzy) or `rtk grep <pattern> [path]` (exact/regex) via Bash for compact RTK output."
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
    "permissionDecisionReason": "Native Grep tool is disabled by RTK policy (default hard deny). Use `rtk rgai <query>` (semantic/fuzzy) or `rtk grep <pattern> [path]` (exact/regex) via Bash. Override: RTK_ALLOW_NATIVE_GREP=1 (legacy: RTK_BLOCK_NATIVE_GREP=0)."
  }
}
EOF
