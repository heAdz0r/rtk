#!/bin/bash
# RTK hook for Claude Code PreToolUse:Read
# Default behavior: allow native Read, but emit guidance to prefer `rtk read`.
# Set RTK_BLOCK_NATIVE_READ=1 to enforce strict blocking.
# Set RTK_NOTIFY_NATIVE_READ=0 to suppress allow-mode notifications.

if [ "${RTK_BLOCK_NATIVE_READ:-0}" != "1" ]; then
  if [ "${RTK_NOTIFY_NATIVE_READ:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Read used. Prefer `rtk read <file> [--level none] [--from N --to M]` via Bash for compact token-aware reads, read-cache reuse, and deterministic exact slices with `--level none`."
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
    "permissionDecisionReason": "Native Read tool is disabled (RTK_BLOCK_NATIVE_READ=1). Use `rtk read <file> [--level none] [--from N --to M]` via Bash instead. Filtered reads are cached; use --level none for exact content."
  }
}
EOF
