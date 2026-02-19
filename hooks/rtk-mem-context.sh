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

# T1.1: activate for ALL Task invocations regardless of subagent_type
# (whitelist removed — new/unknown agent types get memory context too)
if [ "$TOOL_NAME" != "Task" ]; then
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

# If prompt already contains RTK memory preamble, strip it and recover the
# original task body after the separator. This prevents recursive self-noise.
extract_base_prompt() {
  local prompt="$1"
  if [[ "$prompt" == *"RTK Project Memory Context"* ]]; then
    local stripped
    stripped="$(printf "%s" "$prompt" | awk '
      /^---[[:space:]]*$/ { sep=1; next }
      sep { print }
    ')" || true
    if [ -n "${stripped//[$'\n\r\t ']}" ]; then
      printf "%s" "$stripped"
      return
    fi
  fi
  printf "%s" "$prompt"
}

CURRENT_PROMPT=$(echo "$INPUT" | jq -r '.tool_input.prompt // empty')
CURRENT_PROMPT=$(extract_base_prompt "$CURRENT_PROMPT")

# PRD R4: graph-first pipeline — graph seeds + semantic hits + final context
# RTK_MEM_PLAN_BUDGET controls token budget (default 1800 per PRD R4)
RTK_MEM_PLAN_BUDGET="${RTK_MEM_PLAN_BUDGET:-1800}" # ADDED: PRD R4 default budget
MEM_CONTEXT="" # initialize to avoid set -u unbound variable
TASK_HINT=$(printf "%s" "$CURRENT_PROMPT" | tr '\n' ' ' | head -c 700)

# Stage 1: graph-first plan — single call, extract both paths and seeds (PRD R4, CHANGED: single call to avoid double latency)
PLAN_OUTPUT=""
CANDIDATES=""
GRAPH_SEEDS=""
if [ -n "$TASK_HINT" ]; then
  PLAN_OUTPUT=$(rtk memory plan "$TASK_HINT" "$PROJECT_ROOT" \
    --format text --top 25 \
    --token-budget "$RTK_MEM_PLAN_BUDGET" 2>/dev/null) || true
  # Extract file paths (lines starting with spaces and score bracket like "  [0.62] src/...")
  CANDIDATES=$(printf "%s" "$PLAN_OUTPUT" | grep -E "^  \[" | sed "s/^  \[[^]]*\] //" || true)
  GRAPH_SEEDS="$PLAN_OUTPUT"
fi

# Stage 2: semantic search within candidates (actual code snippets, not file list)
SEMANTIC_HITS=""
if [ -n "$CANDIDATES" ]; then
  FILES_CSV=$(printf "%s" "$CANDIDATES" | paste -sd "," -)
  SEMANTIC_HITS=$(rtk rgai "$TASK_HINT" \
    --path "$PROJECT_ROOT" \
    --files "$FILES_CSV" \
    --compact --max 8 2>/dev/null) || true
fi

# Assemble 3-section memory context (PRD R4: Graph Seeds / Semantic Hits / Final Context Files)
if [ -n "$GRAPH_SEEDS" ] || [ -n "$SEMANTIC_HITS" ]; then
  MEM_CONTEXT="### Graph Seeds (graph-first pipeline)
${GRAPH_SEEDS:-none}

### Semantic Hits (candidate-scoped search)
${SEMANTIC_HITS:-none}

### Final Context Files
${CANDIDATES:-none}"
fi

# Fallback: structural explore context when pipeline produces no output (PRD R4 fail-open)
if [ -z "$MEM_CONTEXT" ]; then
  MEM_CONTEXT=$(rtk memory explore "$PROJECT_ROOT" 2>/dev/null) || true # ADDED: fail-open fallback
fi

if [ -z "$MEM_CONTEXT" ]; then
  exit 0
fi

# Inject RTK rules + memory context as prefix to the task prompt
NEW_PROMPT="## RTK Tool Rules (MANDATORY — applies to ALL agents in this session)

Search: ALWAYS use \`rtk rgai\` (semantic) or \`rtk grep\` (regex) — NEVER native Grep tool.
Read:   ALWAYS use \`rtk read <file> --level none\` — NEVER native Read tool.
Write:  ALWAYS use \`rtk write patch/replace/batch\` — NEVER native Edit/Write tools.
Commands: prefix all shell commands with \`rtk\` when an rtk wrapper exists (rtk cargo, rtk bun, etc).

## RTK Project Memory Context (pre-indexed, no need to re-read project files)

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
      "permissionDecisionReason": "RTK memory context injected",
      "updatedInput": $updated
    }
  }'
