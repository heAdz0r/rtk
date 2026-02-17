#!/bin/bash
# RTK Memory Context Hook — PreToolUse:Task
# Injects cached project memory into Explore subagent prompts.
# Wired via: rtk memory install-hook
#
# Input:  JSON from stdin (Claude Code tool_use event)
# Output: JSON with updatedInput (modified task prompt) or nothing (pass-through)

# Guards: dependencies required
if ! command -v rtk &>/dev/null || ! command -v jq &>/dev/null; then
  exit 0
fi

set -euo pipefail

INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
SUBAGENT_TYPE=$(echo "$INPUT" | jq -r '.tool_input.subagent_type // empty' 2>/dev/null)

# Only activate for Explore subagent
if [ "$TOOL_NAME" != "Task" ] || [ "$SUBAGENT_TYPE" != "Explore" ]; then
  exit 0
fi

# Detect project root (walk up from CWD to find .git)
PROJECT_ROOT="."
dir="$PWD"
while [ "$dir" != "/" ]; do
  if [ -d "$dir/.git" ]; then
    PROJECT_ROOT="$dir"
    break
  fi
  dir=$(dirname "$dir")
done

# Get memory context (compact text; cache-hit < 50ms, miss triggers indexing)
MEM_CONTEXT=$(rtk memory explore "$PROJECT_ROOT" 2>/dev/null) || true

if [ -z "$MEM_CONTEXT" ]; then
  exit 0
fi

# Inject memory context as prefix to the task prompt
CURRENT_PROMPT=$(echo "$INPUT" | jq -r '.tool_input.prompt // empty')
NEW_PROMPT="## RTK Project Memory Context (pre-indexed, no need to re-read files)

$MEM_CONTEXT

---

$CURRENT_PROMPT"

ORIGINAL_INPUT=$(echo "$INPUT" | jq -c '.tool_input')
UPDATED_INPUT=$(echo "$ORIGINAL_INPUT" | jq --arg p "$NEW_PROMPT" '.prompt = $p')

jq -n \
  --argjson updated "$UPDATED_INPUT" \
  '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": "RTK memory context injected for Explore agent",
      "updatedInput": $updated
    }
  }'
