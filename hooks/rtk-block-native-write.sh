#!/bin/bash
# RTK block hook for Claude Code PreToolUse:Edit/Write
# Blocks native Edit and Write tools and instructs Claude to use rtk write via Bash instead.
# This ensures file modifications go through RTK for atomic writes, tracking, and token savings.

# Output deny decision with guidance (canonical schema per Context7 docs)
cat <<'EOF'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Native Edit/Write tools are disabled. Use `rtk write` via Bash instead:\n  rtk write replace <file> --from 'old' --to 'new' [--all]\n  rtk write patch <file> --old 'old block' --new 'new block' [--all]\n  rtk write set <file.json> --key a.b --value true\n  rtk write batch --plan '[{\"op\":\"replace\",\"file\":\"...\",\"from\":\"...\",\"to\":\"...\"}]'\nAll writes are atomic (tempfile+rename), idempotent, and support --dry-run."
  }
}
EOF
