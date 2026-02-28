#!/bin/bash
# RTK hook for Claude Code PreToolUse:Read
# Deny-with-content: intercept native Read, run rtk read, return content in deny message.
# Eliminates denial+retry cycle: subagent gets file data in 1 call instead of 2.
#
# Smart level selection (US-001):
#   - Range read (--from/--to specified)          → --level none  (edit mode)
#   - Code files (.go .rs .py .ts .js .java ...)  → --level minimal
#   - Config/data (.json .yaml .toml .env .lock)  → --level none
#   - Docs (.md .txt .rst)                        → --level minimal
#   - Unknown extension                           → --level minimal
#   - Fallback: if minimal returns empty          → retry --level none
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
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Native Read blocked. Use Bash: rtk read <file> [--level minimal|none] [--from N --to M].\n  Overview/understanding → --level minimal\n  Editing (with --from/--to) → --level none"}}
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

# Whitelist: Claude Code internal memory/project files (allow native read passthrough)
if [[ "$FILE_PATH" == *"/.claude/projects/"*"/memory/"* ]] || \
   [[ "$FILE_PATH" == *"/.claude/CLAUDE.md" ]] || \
   [[ "$FILE_PATH" == *"/.claude/settings.json" ]]; then
  exit 0
fi

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
  jq -n --arg r "Native Read blocked. File not found: ${FILE_PATH:-<empty>}." \
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
  exit 0
fi

# Determine if this is a range read (edit mode) — always use --level none
HAS_RANGE=0
RANGE_ARGS=""
if [ -n "$OFFSET" ] && [ "$OFFSET" != "null" ] && [ "$OFFSET" != "0" ]; then
  HAS_RANGE=1
  RANGE_ARGS="--from $OFFSET"
fi
if [ -n "$LIMIT" ] && [ "$LIMIT" != "null" ]; then
  if [ -n "$OFFSET" ] && [ "$OFFSET" != "null" ] && [ "$OFFSET" != "0" ]; then
    TO_LINE=$((OFFSET + LIMIT))
  else
    TO_LINE=$LIMIT
    HAS_RANGE=1
  fi
  RANGE_ARGS="$RANGE_ARGS --to $TO_LINE"
fi

# Smart level selection by file extension (US-001)
# Range reads always get --level none (editing needs full context)
FILE_EXT="${FILE_PATH##*.}"
FILE_EXT_LOWER=$(echo "$FILE_EXT" | tr '[:upper:]' '[:lower:]')

if [ "$HAS_RANGE" = "1" ]; then
  SMART_LEVEL="none"
else
  case "$FILE_EXT_LOWER" in
    go|rs|py|pyw|ts|tsx|js|jsx|mjs|cjs|java|rb|sh|bash|zsh|c|h|cpp|cc|cxx|hpp)
      SMART_LEVEL="minimal" ;;
    json|yaml|yml|toml|env|lock|mod|sum|ini|cfg|conf)
      SMART_LEVEL="none" ;;
    md|txt|rst|csv|log)
      SMART_LEVEL="minimal" ;;
    *)
      SMART_LEVEL="minimal" ;;
  esac
fi

RTK_ARGS="--level $SMART_LEVEL $RANGE_ARGS"

# Execute rtk read with smart level (timeout 5s, cap at 50K chars)
CONTENT=$(timeout 5 rtk read "$FILE_PATH" $RTK_ARGS 2>/dev/null | head -c 50000) || CONTENT=""

# Two-pass fallback: if smart level returns empty and wasn't already none, retry with none (US-001)
USED_LEVEL="$SMART_LEVEL"
if [ -z "$CONTENT" ] && [ "$SMART_LEVEL" != "none" ]; then
  RTK_ARGS="--level none $RANGE_ARGS"
  CONTENT=$(timeout 5 rtk read "$FILE_PATH" $RTK_ARGS 2>/dev/null | head -c 50000) || CONTENT=""
  USED_LEVEL="none (fallback)"
fi

if [ -z "$CONTENT" ]; then
  jq -n --arg r "Native Read blocked. rtk read returned empty for: $FILE_PATH.\nUse: rtk read \"$FILE_PATH\" --level none\nHint: --level none only needed for editing; use --level minimal for overview." \
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
  exit 0
fi

# Deliver content in deny message (filtered: $USED_LEVEL shown in header)
REASON=$(printf 'INTERCEPTED (filtered: %s): content delivered via rtk read (do NOT retry this read).\n--- %s ---\n%s\n--- end ---' "$USED_LEVEL" "$FILE_PATH" "$CONTENT")

jq -n --arg r "$REASON" \
  '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":$r}}'
