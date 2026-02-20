#!/bin/bash
# RTK hook for Claude Code PreToolUse:Edit/Write
# Default behavior: hard deny native Edit/Write and route to `rtk write`.
# Set RTK_ALLOW_NATIVE_WRITE=1 (or legacy RTK_BLOCK_NATIVE_WRITE=0) to allow native Edit/Write.
# Set RTK_NOTIFY_NATIVE_WRITE=0 to suppress allow-mode notifications.

# Explicit allow override (policy default is deny)
if [ "${RTK_ALLOW_NATIVE_WRITE:-0}" = "1" ] || [ "${RTK_BLOCK_NATIVE_WRITE:-1}" = "0" ]; then
  if [ "${RTK_NOTIFY_NATIVE_WRITE:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF_JSON'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Edit/Write allowed by override. Prefer Bash: rtk write replace|patch|set|batch."
  }
}
EOF_JSON
  exit 0
fi

cat <<'EOF_JSON'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Edit/Write blocked by RTK policy. Use Bash: rtk write replace|patch|set|batch. Override: RTK_ALLOW_NATIVE_WRITE=1 (RTK_BLOCK_NATIVE_WRITE=0)."
  }
}
EOF_JSON
