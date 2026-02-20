#!/bin/bash
# RTK hook for Claude Code PreToolUse:Grep
# Deny-with-content: intercept native Grep, run rtk grep, return results in deny message.
# Eliminates denial+retry cycle for search operations.
#
# Set RTK_ALLOW_NATIVE_GREP=1 to bypass.
# Set RTK_GREP_DENY_PLAIN=1 for old behavior.

# Allow override
if [ "${RTK_ALLOW_NATIVE_GREP:-0}" = "1" ] || [ "${RTK_BLOCK_NATIVE_GREP:-1}" = "0" ]; then
  [ "${RTK_NOTIFY_NATIVE_GREP:-1}" = "0" ] && exit 0
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"Native Grep allowed by override."}}
EOF_JSON
  exit 0
fi

# Plain deny
if [ "${RTK_GREP_DENY_PLAIN:-0}" = "1" ]; then
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Grep blocked. Use Bash: rtk rgai <query> or rtk grep <pattern>."}}
EOF_JSON
  exit 0
fi

# --- Deny-with-content mode ---
if ! command -v jq &>/dev/null || ! command -v rtk &>/dev/null; then
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Grep blocked (jq/rtk unavailable). Use Bash: rtk grep <pattern>."}}
EOF_JSON
  exit 0
fi

INPUT=$(cat)
PATTERN=$(echo "$INPUT" | jq -r '.tool_input.pattern // empty' 2>/dev/null)
SEARCH_PATH=$(echo "$INPUT" | jq -r '.tool_input.path // "."' 2>/dev/null)

if [ -z "$PATTERN" ]; then
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Grep blocked. Empty pattern."}}
EOF_JSON
  exit 0
fi

# Run rtk grep with the original pattern (timeout 5s, cap output)
CONTENT=$(timeout 5 rtk grep "$PATTERN" "$SEARCH_PATH" 2>/dev/null | head -c 30000) || CONTENT=""

if [ -z "$CONTENT" ]; then
  # Fallback: try rtk rgai for semantic match
  CONTENT=$(timeout 5 rtk rgai "$PATTERN" --compact 2>/dev/null | head -c 30000) || CONTENT=""
fi

if [ -z "$CONTENT" ]; then
  jq -n --arg r "Native Grep blocked. No results for: $PATTERN. Try: rtk grep \"$PATTERN\" or rtk rgai \"$PATTERN\"" \
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
  exit 0
fi

REASON=$(printf 'INTERCEPTED: search results via rtk (do NOT retry this search).\n--- grep "%s" ---\n%s\n--- end ---' "$PATTERN" "$CONTENT")

jq -n --arg r "$REASON" \
  '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
