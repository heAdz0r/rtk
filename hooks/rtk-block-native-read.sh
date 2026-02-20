#!/bin/bash
# RTK hook for Claude Code PreToolUse:Read
# Deny-with-content: intercept native Read, run rtk read, return content in deny message.
# Eliminates denial+retry cycle: subagent gets file data in 1 call instead of 2.
#
# Set RTK_ALLOW_NATIVE_READ=1 to bypass (allow native Read).
# Set RTK_READ_DENY_PLAIN=1 for old behavior (plain deny, no content).

# Allow override
if [ "${RTK_ALLOW_NATIVE_READ:-0}" = "1" ] || [ "${RTK_BLOCK_NATIVE_READ:-1}" = "0" ]; then
  [ "${RTK_NOTIFY_NATIVE_READ:-1}" = "0" ] && exit 0
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"Native Read allowed by override."}}
EOF_JSON
  exit 0
fi

# Plain deny (no content)
if [ "${RTK_READ_DENY_PLAIN:-0}" = "1" ]; then
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Read blocked. Use Bash: rtk read <file> --level none [--from N --to M]."}}
EOF_JSON
  exit 0
fi

# --- Deny-with-content mode ---
if ! command -v jq &>/dev/null || ! command -v rtk &>/dev/null; then
  cat <<'EOF_JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Read blocked (jq/rtk unavailable). Use Bash: rtk read <file>."}}
EOF_JSON
  exit 0
fi

INPUT=$(cat)
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
OFFSET=$(echo "$INPUT" | jq -r '.tool_input.offset // empty' 2>/dev/null)
LIMIT=$(echo "$INPUT" | jq -r '.tool_input.limit // empty' 2>/dev/null)

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
  jq -n --arg r "Native Read blocked. File not found: ${FILE_PATH:-<empty>}." \
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
  exit 0
fi

# Build rtk read command matching the original Read parameters
RTK_ARGS="--level none"
if [ -n "$OFFSET" ] && [ "$OFFSET" != "null" ] && [ "$OFFSET" != "0" ]; then
  RTK_ARGS="$RTK_ARGS --from $OFFSET"
fi
if [ -n "$LIMIT" ] && [ "$LIMIT" != "null" ]; then
  if [ -n "$OFFSET" ] && [ "$OFFSET" != "null" ] && [ "$OFFSET" != "0" ]; then
    TO_LINE=$((OFFSET + LIMIT))
  else
    TO_LINE=$LIMIT
  fi
  RTK_ARGS="$RTK_ARGS --to $TO_LINE"
fi

# Execute rtk read (timeout 5s, cap at 50K chars to stay within context limits)
CONTENT=$(timeout 5 rtk read "$FILE_PATH" $RTK_ARGS 2>/dev/null | head -c 50000) || CONTENT=""

if [ -z "$CONTENT" ]; then
  jq -n --arg r "Native Read blocked. rtk read returned empty for: $FILE_PATH. Try: rtk read \"$FILE_PATH\" --level none" \
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
  exit 0
fi

# Deliver content in deny message — subagent gets data without retry
REASON=$(printf 'INTERCEPTED: content delivered via rtk read (do NOT retry this read).\n--- %s ---\n%s\n--- end ---' "$FILE_PATH" "$CONTENT")

jq -n --arg r "$REASON" \
  '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
