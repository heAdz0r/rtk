#!/bin/bash
# RTK hook for Claude Code PreToolUse:Edit/Write
# Default behavior: allow native Edit/Write, but emit guidance to prefer `rtk write`.
# Set RTK_BLOCK_NATIVE_WRITE=1 to enforce strict blocking.
# Set RTK_NOTIFY_NATIVE_WRITE=0 to suppress allow-mode notifications.

if [ "${RTK_BLOCK_NATIVE_WRITE:-0}" != "1" ]; then
  if [ "${RTK_NOTIFY_NATIVE_WRITE:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Edit/Write used. Prefer `rtk write` via Bash for atomic/idempotent writes and better agent consistency:\n  rtk write replace <file> --from 'old' --to 'new' [--all] [--cas] [--retry N]\n  rtk write patch <file> --old 'old block' --new 'new block' [--all] [--cas] [--retry N]\n  rtk write set <file.json> --key a.b --value true [--cas] [--retry N]\n  rtk write batch --plan '[{\"op\":\"replace\",\"file\":\"...\",\"from\":\"...\",\"to\":\"...\"}]'"
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
    "permissionDecisionReason": "Native Edit/Write tools are disabled (RTK_BLOCK_NATIVE_WRITE=1). Use `rtk write` via Bash instead:\n  rtk write replace <file> --from 'old' --to 'new' [--all] [--cas] [--retry N]\n  rtk write patch <file> --old 'old block' --new 'new block' [--all] [--cas] [--retry N]\n  rtk write set <file.json> --key a.b --value true [--cas] [--retry N]\n  rtk write batch --plan '[{\"op\":\"replace\",\"file\":\"...\",\"from\":\"...\",\"to\":\"...\"}]'"
  }
}
EOF
