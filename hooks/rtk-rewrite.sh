#!/bin/bash
# RTK auto-rewrite hook for Claude Code PreToolUse:Bash
# Transparently rewrites raw commands to their rtk equivalents.
# Outputs JSON with updatedInput to modify the command before execution.

# --- Audit logging (opt-in via RTK_HOOK_AUDIT=1) ---
_rtk_audit_log() {
  if [ "${RTK_HOOK_AUDIT:-0}" != "1" ]; then return; fi
  local action="$1" original="$2" rewritten="${3:--}" class="${4:-unknown}"
  local dir="${RTK_AUDIT_DIR:-${HOME}/.local/share/rtk}"
  mkdir -p "$dir"
  printf '%s | %s | class=%s | %s | %s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$action" "$class" "$original" "$rewritten" \
    >> "${dir}/hook-audit.log"
}

# Guards: skip silently if dependencies missing
if ! command -v rtk &>/dev/null || ! command -v jq &>/dev/null; then
  _rtk_audit_log "skip:no_deps" "-" "-" "unknown"
  exit 0
fi

set -euo pipefail

INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty')

if [ -z "$CMD" ]; then
  _rtk_audit_log "skip:empty" "-" "-" "unknown"
  exit 0
fi

# Extract the first meaningful command (before pipes, &&, etc.)
# We only rewrite if the FIRST command in a chain matches.
FIRST_CMD="$CMD"
CMD_CLASS="unknown"
FIRST_TOKEN=$(echo "$FIRST_CMD" | awk '{print $1}')

# Skip if already using rtk
case "$FIRST_TOKEN" in
  rtk|*/rtk) _rtk_audit_log "skip:already_rtk" "$CMD" "-" "$CMD_CLASS"; exit 0 ;;
esac

# Skip commands with heredocs, variable assignments as the whole command, etc.
case "$FIRST_CMD" in
  *'<<'*) _rtk_audit_log "skip:heredoc" "$CMD" "-" "$CMD_CLASS"; exit 0 ;;
esac

# Strip leading env var assignments for pattern matching
# e.g., "TEST_SESSION_ID=2 npx playwright test" → match against "npx playwright test"
# but preserve them in the rewritten command for execution.
ENV_PREFIX=$(echo "$FIRST_CMD" | grep -oE '^([A-Za-z_][A-Za-z0-9_]*=[^ ]* +)+' || echo "")
if [ -n "$ENV_PREFIX" ]; then
  MATCH_CMD="${FIRST_CMD:${#ENV_PREFIX}}"
  CMD_BODY="${CMD:${#ENV_PREFIX}}"
else
  MATCH_CMD="$FIRST_CMD"
  CMD_BODY="$CMD"
fi

REWRITTEN=""
ALLOW_MUTATING="${RTK_REWRITE_MUTATING:-0}"

# Fix: strip leading comment/blank lines so comment-prefixed commands are still rewritten.
# e.g. "# explain\ncat file" -> MATCH_CMD="cat file", enables pattern matching.
if echo "$MATCH_CMD" | head -1 | grep -qE '^[[:space:]]*#'; then
  MATCH_CMD=$(echo "$MATCH_CMD" | awk '!/^[[:space:]]*($|#)/{found=1} found{print}')
  CMD_BODY=$(echo "$CMD_BODY" | awk '!/^[[:space:]]*($|#)/{found=1} found{print}')
  FIRST_TOKEN=$(echo "$MATCH_CMD" | awk '{print $1}')
  # Re-check: if already rtk after stripping comments, pass through
  case "$FIRST_TOKEN" in
    rtk|*/rtk) _rtk_audit_log "skip:already_rtk" "$CMD" "-" "$CMD_CLASS"; exit 0 ;;
  esac
fi

# --- Git commands ---
if echo "$MATCH_CMD" | grep -qE '^git[[:space:]]'; then
  GIT_SUBCMD=$(echo "$MATCH_CMD" | sed -E \
    -e 's/^git[[:space:]]+//' \
    -e 's/(-C|-c)[[:space:]]+[^[:space:]]+[[:space:]]*//g' \
    -e 's/--[a-z-]+=[^[:space:]]+[[:space:]]*//g' \
    -e 's/--(no-pager|no-optional-locks|bare|literal-pathspecs)[[:space:]]*//g' \
    -e 's/^[[:space:]]+//')
  GIT_VERB=$(echo "$GIT_SUBCMD" | awk '{print $1}')
  case "$GIT_VERB" in
    status|diff|log|show|remote|merge-base|rev-parse|ls-files) CMD_CLASS="read_only" ;;
    add|commit|push|pull|fetch|checkout|cherry-pick|merge|rebase|rm) CMD_CLASS="mutating" ;;
    branch)
      BRANCH_ARGS="${GIT_SUBCMD#branch}"
      if echo "$BRANCH_ARGS" | grep -qE '(^|[[:space:]])-(d|D|m|M|c|C)([[:space:]]|$)'; then
        CMD_CLASS="mutating"
      elif echo "$BRANCH_ARGS" | awk '{for (i=1;i<=NF;i++) if ($i !~ /^-/) {print "yes"; exit}}' | grep -q "yes"; then
        # Safe default: `git branch foo` may mutate state, so treat as mutating.
        CMD_CLASS="mutating"
      else
        CMD_CLASS="read_only"
      fi
      ;;
    stash)
      STASH_SUBCMD=$(echo "$GIT_SUBCMD" | awk '{print $2}')
      case "$STASH_SUBCMD" in
        list|show) CMD_CLASS="read_only" ;;
        "") CMD_CLASS="mutating" ;;
        *) CMD_CLASS="mutating" ;;
      esac
      ;;
    worktree)
      WORKTREE_SUBCMD=$(echo "$GIT_SUBCMD" | awk '{print $2}')
      case "$WORKTREE_SUBCMD" in
        add|remove|prune|lock|unlock|move) CMD_CLASS="mutating" ;;
        *) CMD_CLASS="read_only" ;;
      esac
      ;;
    *) CMD_CLASS="unknown" ;;
  esac

  case "$GIT_SUBCMD" in
    status|status\ *|diff|diff\ *|log|log\ *|add|add\ *|commit|commit\ *|push|push\ *|pull|pull\ *|branch|branch\ *|fetch|fetch\ *|stash|stash\ *|show|show\ *|checkout|checkout\ *|cherry-pick|cherry-pick\ *|remote|remote\ *|merge-base|merge-base\ *|rev-parse|rev-parse\ *|ls-files|ls-files\ *|merge|merge\ *|rebase|rebase\ *|rm|rm\ *)
      REWRITTEN="${ENV_PREFIX}rtk $CMD_BODY"
      ;;
  esac

  if [ -n "$REWRITTEN" ] && [ "$CMD_CLASS" = "mutating" ] && [ "$ALLOW_MUTATING" != "1" ]; then
    _rtk_audit_log "skip:mutating_guard" "$CMD" "-" "$CMD_CLASS"
    exit 0
  fi

# --- GitHub CLI (added: api, release) ---
elif echo "$MATCH_CMD" | grep -qE '^gh[[:space:]]+(pr|issue|run|api|release|repo)([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^gh /rtk gh /')"

# --- cargo install --path <rtk-root>: full install to ALL binary locations (no desync) ---
elif echo "$MATCH_CMD" | grep -qE '^([^[:space:]]*/)?cargo[[:space:]]+install[[:space:]].*--path[[:space:]]'; then
  _RTK_INSTALL_ARG=$(echo "$MATCH_CMD" | grep -oE '\-\-path[[:space:]]+[^[:space:]]+' | head -1 | sed 's/--path[[:space:]]*//')
  if [ -z "$_RTK_INSTALL_ARG" ] || [ "$_RTK_INSTALL_ARG" = "." ]; then
    _RTK_RESOLVED="$PWD"
  else
    _RTK_RESOLVED="$(cd "$_RTK_INSTALL_ARG" 2>/dev/null && pwd || echo "$_RTK_INSTALL_ARG")"
  fi
  if [ -f "$_RTK_RESOLVED/Cargo.toml" ] && grep -q 'name = "rtk"' "$_RTK_RESOLVED/Cargo.toml" 2>/dev/null; then
    # RTK project: full install to ~/.cargo/bin + /usr/local/bin (eliminates desync)
    CMD_CLASS="mutating"
    REWRITTEN="rtk build sh --root $_RTK_RESOLVED --no-debug --no-verify"
  else
    # Other project: normal cargo install passthrough
    CMD_CLASS="mutating"
    REWRITTEN="${ENV_PREFIX}rtk $(echo "$CMD_BODY" | sed -E 's|^([^[:space:]]*/)?cargo[[:space:]]+|cargo |')"
  fi

# --- Cargo ---
elif echo "$MATCH_CMD" | grep -qE '^([^[:space:]]*/)?cargo[[:space:]]'; then
  CMD_CLASS="read_only"
  CARGO_SUBCMD=$(echo "$MATCH_CMD" | sed -E 's|^([^[:space:]]*/)?cargo[[:space:]]+(\+[^[:space:]]+[[:space:]]+)?||')
  CANON_CARGO_BODY=$(echo "$CMD_BODY" | sed -E 's|^([^[:space:]]*/)?cargo[[:space:]]+|cargo |')
  case "$CARGO_SUBCMD" in
    test|test\ *|build|build\ *|clippy|clippy\ *|check|check\ *|install|install\ *|nextest|nextest\ *|fmt|fmt\ *|run|run\ *)
      REWRITTEN="${ENV_PREFIX}rtk $CANON_CARGO_BODY"
      ;;
  esac

# --- Semantic search (fork-specific: rgai/grepai) ---
# Priority: rtk rgai (fuzzy/semantic) > rtk grep (exact/regex)
# Use rgai first for intent-based discovery, grep for precise matches
elif echo "$MATCH_CMD" | grep -qE '^([^[:space:]]*/)?(grepai|rgai)[[:space:]]+search([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^([^[:space:]]*\/)?(grepai|rgai)[[:space:]]+search[[:space:]]+/rtk rgai /')"
elif echo "$MATCH_CMD" | grep -qE '^([^[:space:]]*/)?rgai[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's|^([^[:space:]]*/)?rgai[[:space:]]+|rtk rgai |')"

# --- File operations ---
elif echo "$MATCH_CMD" | grep -qE '^cat[[:space:]]+'; then
  CMD_CLASS="read_only"
  CAT_ARGS=$(echo "$MATCH_CMD" | sed -E 's/^cat[[:space:]]*//')
  # Only rewrite simple `cat <single-file>` without flags or multiple files
  if ! echo "$CAT_ARGS" | grep -qE '^-' && ! echo "$CAT_ARGS" | grep -qE ' '; then
    REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^cat /rtk read /')"
  fi
elif echo "$MATCH_CMD" | grep -qE '^(rg|grep)[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(rg|grep) /rtk grep /')"
elif echo "$MATCH_CMD" | grep -qE '^ls([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^ls/rtk ls/')"
elif echo "$MATCH_CMD" | grep -qE '^tree([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^tree/rtk tree/')"
elif echo "$MATCH_CMD" | grep -qE '^find[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^find /rtk find /')"
elif echo "$MATCH_CMD" | grep -qE '^diff[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^diff /rtk diff /')"
elif echo "$MATCH_CMD" | grep -qE '^head[[:space:]]+'; then
  CMD_CLASS="read_only"
  # Transform: head -N file → rtk read file --max-lines N
  # Also handle: head --lines=N file
  if echo "$MATCH_CMD" | grep -qE '^head[[:space:]]+-[0-9]+[[:space:]]+'; then
    LINES=$(echo "$MATCH_CMD" | sed -E 's/^head +-([0-9]+) +.+$/\1/')
    FILE=$(echo "$MATCH_CMD" | sed -E 's/^head +-[0-9]+ +(.+)$/\1/')
    REWRITTEN="${ENV_PREFIX}rtk read $FILE --max-lines $LINES"
  elif echo "$MATCH_CMD" | grep -qE '^head[[:space:]]+--lines=[0-9]+[[:space:]]+'; then
    LINES=$(echo "$MATCH_CMD" | sed -E 's/^head +--lines=([0-9]+) +.+$/\1/')
    FILE=$(echo "$MATCH_CMD" | sed -E 's/^head +--lines=[0-9]+ +(.+)$/\1/')
    REWRITTEN="${ENV_PREFIX}rtk read $FILE --max-lines $LINES"
  fi

# --- Safe writes ---
elif echo "$MATCH_CMD" | grep -qE '^write[[:space:]]+(replace|patch|set)([[:space:]]|$)'; then
  CMD_CLASS="mutating"
  REWRITTEN="${ENV_PREFIX}rtk $CMD_BODY"
elif [[ "$MATCH_CMD" =~ ^sed[[:space:]]+-i([[:space:]]+\'\'|[[:space:]]+\"\")?[[:space:]]+\'s/([^/\']+)/([^/\']+)/([g]?)\'[[:space:]]+([^[:space:]]+)$ ]]; then
  CMD_CLASS="mutating"
  FROM="${BASH_REMATCH[2]}"
  TO="${BASH_REMATCH[3]}"
  FLAGS="${BASH_REMATCH[4]}"
  FILE="${BASH_REMATCH[5]}"
  REWRITTEN="${ENV_PREFIX}rtk write replace $FILE --from '$FROM' --to '$TO' --retry 3" # changed: auto-inject --retry 3 for concurrent safety
  if [ "$FLAGS" = "g" ]; then
    REWRITTEN="$REWRITTEN --all"
  fi
elif [[ "$MATCH_CMD" =~ ^sed[[:space:]]+-i([[:space:]]+\'\'|[[:space:]]+\"\")?[[:space:]]+\"s/([^/\"]+)/([^/\"]+)/([g]?)\"[[:space:]]+([^[:space:]]+)$ ]]; then
  CMD_CLASS="mutating"
  FROM="${BASH_REMATCH[2]}"
  TO="${BASH_REMATCH[3]}"
  FLAGS="${BASH_REMATCH[4]}"
  FILE="${BASH_REMATCH[5]}"
  REWRITTEN="${ENV_PREFIX}rtk write replace $FILE --from '$FROM' --to '$TO' --retry 3" # changed: auto-inject --retry 3 for concurrent safety
  if [ "$FLAGS" = "g" ]; then
    REWRITTEN="$REWRITTEN --all"
  fi
elif [[ "$MATCH_CMD" =~ ^perl[[:space:]]+-pi[[:space:]]+-e[[:space:]]+\'s/([^/\']+)/([^/\']+)/([g]?)\'[[:space:]]+([^[:space:]]+)$ ]]; then
  CMD_CLASS="mutating"
  FROM="${BASH_REMATCH[1]}"
  TO="${BASH_REMATCH[2]}"
  FLAGS="${BASH_REMATCH[3]}"
  FILE="${BASH_REMATCH[4]}"
  REWRITTEN="${ENV_PREFIX}rtk write replace $FILE --from '$FROM' --to '$TO' --retry 3" # changed: auto-inject --retry 3 for concurrent safety
  if [ "$FLAGS" = "g" ]; then
    REWRITTEN="$REWRITTEN --all"
  fi

# --- JS/TS tooling (added: npm run, npm test, vue-tsc) ---
elif echo "$MATCH_CMD" | grep -qE '^(pnpm[[:space:]]+)?(npx[[:space:]]+)?vitest([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(pnpm )?(npx )?vitest( run)?/rtk vitest run/')"
elif echo "$MATCH_CMD" | grep -qE '^pnpm[[:space:]]+test([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pnpm test/rtk vitest run/')"
elif echo "$MATCH_CMD" | grep -qE '^npm[[:space:]]+test([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^npm test/rtk npm test/')"
elif echo "$MATCH_CMD" | grep -qE '^npm[[:space:]]+run[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^npm run /rtk npm /')"
elif echo "$MATCH_CMD" | grep -qE '^bun[[:space:]]+'; then
  BUN_SUBCMD=$(echo "$MATCH_CMD" | awk '{print $2}')
  case "$BUN_SUBCMD" in
    install|add|remove|update|upgrade|pm)
      CMD_CLASS="mutating"
      ;;
    *)
      CMD_CLASS="read_only"
      REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^bun /rtk bun /')"
      ;;
  esac
elif echo "$MATCH_CMD" | grep -qE '^((npx|bunx)[[:space:]]+)?vue-tsc([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx |bunx )?vue-tsc/rtk npx vue-tsc/')"
elif echo "$MATCH_CMD" | grep -qE '^vue[[:space:]]+tsc([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^vue tsc/rtk npx vue-tsc/')"
elif echo "$MATCH_CMD" | grep -qE '^pnpm[[:space:]]+tsc([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pnpm tsc/rtk tsc/')"
elif echo "$MATCH_CMD" | grep -qE '^(npx[[:space:]]+)?tsc([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx )?tsc/rtk tsc/')"
elif echo "$MATCH_CMD" | grep -qE '^pnpm[[:space:]]+lint([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pnpm lint/rtk lint/')"
elif echo "$MATCH_CMD" | grep -qE '^(npx[[:space:]]+)?eslint([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx )?eslint/rtk lint/')"
elif echo "$MATCH_CMD" | grep -qE '^(npx[[:space:]]+)?prettier([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx )?prettier/rtk prettier/')"
elif echo "$MATCH_CMD" | grep -qE '^(npx[[:space:]]+)?playwright([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx )?playwright/rtk playwright/')"
elif echo "$MATCH_CMD" | grep -qE '^pnpm[[:space:]]+playwright([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pnpm playwright/rtk playwright/')"
elif echo "$MATCH_CMD" | grep -qE '^(npx[[:space:]]+)?prisma([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^(npx )?prisma/rtk prisma/')"

# --- Containers (added: docker compose, docker run/build/exec, kubectl describe/apply) ---
elif echo "$MATCH_CMD" | grep -qE '^docker[[:space:]]'; then
  CMD_CLASS="read_only"
  if echo "$MATCH_CMD" | grep -qE '^docker[[:space:]]+compose([[:space:]]|$)'; then
    REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^docker /rtk docker /')"
  else
    DOCKER_SUBCMD=$(echo "$MATCH_CMD" | sed -E \
      -e 's/^docker[[:space:]]+//' \
      -e 's/(-H|--context|--config)[[:space:]]+[^[:space:]]+[[:space:]]*//g' \
      -e 's/--[a-z-]+=[^[:space:]]+[[:space:]]*//g' \
      -e 's/^[[:space:]]+//')
    case "$DOCKER_SUBCMD" in
      ps|ps\ *|images|images\ *|logs|logs\ *|run|run\ *|build|build\ *|exec|exec\ *)
        REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^docker /rtk docker /')"
        ;;
    esac
  fi
elif echo "$MATCH_CMD" | grep -qE '^kubectl[[:space:]]'; then
  CMD_CLASS="read_only"
  KUBE_SUBCMD=$(echo "$MATCH_CMD" | sed -E \
    -e 's/^kubectl[[:space:]]+//' \
    -e 's/(--context|--kubeconfig|--namespace|-n)[[:space:]]+[^[:space:]]+[[:space:]]*//g' \
    -e 's/--[a-z-]+=[^[:space:]]+[[:space:]]*//g' \
    -e 's/^[[:space:]]+//')
  case "$KUBE_SUBCMD" in
    get|get\ *|logs|logs\ *|describe|describe\ *|apply|apply\ *)
      REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^kubectl /rtk kubectl /')"
      ;;
  esac

# --- Network ---
elif echo "$MATCH_CMD" | grep -qE '^ssh([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^ssh/rtk ssh/')"
elif echo "$MATCH_CMD" | grep -qE '^curl[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^curl /rtk curl /')"
elif echo "$MATCH_CMD" | grep -qE '^wget[[:space:]]+'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^wget /rtk wget /')"

# --- pnpm package management ---
elif echo "$MATCH_CMD" | grep -qE '^pnpm[[:space:]]+(list|ls|outdated)([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pnpm /rtk pnpm /')"

# --- Python tooling ---
elif echo "$MATCH_CMD" | grep -qE '^pytest([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^pytest/rtk pytest/')"
elif echo "$MATCH_CMD" | grep -qE '^python3?[[:space:]]+-m[[:space:]]+pytest([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's/^python3?[[:space:]]+-m[[:space:]]+pytest/rtk pytest/')"
elif echo "$MATCH_CMD" | grep -qE '^ruff[[:space:]]+(check|format)([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^ruff /rtk ruff /')"
elif echo "$MATCH_CMD" | grep -qE '^([^[:space:]]*/)?pip[[:space:]]+(list|outdated|install|show|uninstall)([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed -E 's|^([^[:space:]]*/)?pip[[:space:]]+|rtk pip |')"
elif echo "$MATCH_CMD" | grep -qE '^uv[[:space:]]+pip[[:space:]]+(list|outdated|install|show)([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^uv pip /rtk pip /')"

# --- Go tooling ---
elif echo "$MATCH_CMD" | grep -qE '^go[[:space:]]+test([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^go test/rtk go test/')"
elif echo "$MATCH_CMD" | grep -qE '^go[[:space:]]+build([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^go build/rtk go build/')"
elif echo "$MATCH_CMD" | grep -qE '^go[[:space:]]+vet([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^go vet/rtk go vet/')"
elif echo "$MATCH_CMD" | grep -qE '^golangci-lint([[:space:]]|$)'; then
  CMD_CLASS="read_only"
  REWRITTEN="${ENV_PREFIX}$(echo "$CMD_BODY" | sed 's/^golangci-lint/rtk golangci-lint/')"
fi

# If no rewrite needed, approve as-is
if [ -z "$REWRITTEN" ]; then
  _rtk_audit_log "skip:no_match" "$CMD" "-" "$CMD_CLASS"
  exit 0
fi

_rtk_audit_log "rewrite" "$CMD" "$REWRITTEN" "$CMD_CLASS"

# Build the updated tool_input with all original fields preserved, only command changed
ORIGINAL_INPUT=$(echo "$INPUT" | jq -c '.tool_input')
UPDATED_INPUT=$(echo "$ORIGINAL_INPUT" | jq --arg cmd "$REWRITTEN" '.command = $cmd')

# Output the rewrite instruction
jq -n \
  --argjson updated "$UPDATED_INPUT" \
  '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": "RTK auto-rewrite",
      "updatedInput": $updated
    }
  }'
