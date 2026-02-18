#!/bin/bash
# RTK hook for Claude Code PreToolUse:Read
# Default behavior: hard deny native Read and route to `rtk read`.
# Set RTK_ALLOW_NATIVE_READ=1 (or legacy RTK_BLOCK_NATIVE_READ=0) to allow native Read.
# Set RTK_NOTIFY_NATIVE_READ=0 to suppress allow-mode notifications.

# Explicit allow override (policy default is deny)
if [ "${RTK_ALLOW_NATIVE_READ:-0}" = "1" ] || [ "${RTK_BLOCK_NATIVE_READ:-1}" = "0" ]; then
  if [ "${RTK_NOTIFY_NATIVE_READ:-1}" = "0" ]; then
    exit 0
  fi

  cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Native Read explicitly allowed by policy override. Prefer `rtk read <file> [--level none] [--from N --to M]` via Bash for compact token-aware reads, read-cache reuse, and deterministic exact slices with `--level none`."
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
    "permissionDecisionReason": "Native Read tool is disabled by RTK policy (default hard deny). Use `rtk read <file> [--level none] [--from N --to M]` via Bash instead. Override: RTK_ALLOW_NATIVE_READ=1 (legacy: RTK_BLOCK_NATIVE_READ=0)."
  }
}
EOF
