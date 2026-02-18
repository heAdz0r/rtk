#!/bin/bash
# Test suite for rtk-rewrite.sh
# Feeds mock JSON through the hook and verifies the rewritten commands.
#
# Usage: bash ~/.claude/hooks/test-rtk-rewrite.sh

HOOK="${HOOK:-$HOME/.claude/hooks/rtk-rewrite.sh}"
PASS=0
FAIL=0
TOTAL=0

# Colors
GREEN='\033[32m'
RED='\033[31m'
DIM='\033[2m'
RESET='\033[0m'

test_rewrite() {
  test_rewrite_with_env "$1" "$2" "$3" ""
}

test_rewrite_with_env() {
  local description="$1"
  local input_cmd="$2"
  local expected_cmd="$3"  # empty string = expect no rewrite
  local env_spec="${4:-}"
  TOTAL=$((TOTAL + 1))

  local input_json
  input_json=$(jq -n --arg cmd "$input_cmd" '{"tool_name":"Bash","tool_input":{"command":$cmd}}')
  local output
  if [ -n "$env_spec" ]; then
    output=$(echo "$input_json" | env $env_spec bash "$HOOK" 2>/dev/null) || true
  else
    output=$(echo "$input_json" | bash "$HOOK" 2>/dev/null) || true
  fi

  if [ -z "$expected_cmd" ]; then
    # Expect no rewrite (hook exits 0 with no output)
    if [ -z "$output" ]; then
      printf "  ${GREEN}PASS${RESET} %s ${DIM}→ (no rewrite)${RESET}\n" "$description"
      PASS=$((PASS + 1))
    else
      local actual
      actual=$(echo "$output" | jq -r '.hookSpecificOutput.updatedInput.command // empty')
      printf "  ${RED}FAIL${RESET} %s\n" "$description"
      printf "       expected: (no rewrite)\n"
      printf "       actual:   %s\n" "$actual"
      FAIL=$((FAIL + 1))
    fi
  else
    local actual
    actual=$(echo "$output" | jq -r '.hookSpecificOutput.updatedInput.command // empty' 2>/dev/null)
    if [ "$actual" = "$expected_cmd" ]; then
      printf "  ${GREEN}PASS${RESET} %s ${DIM}→ %s${RESET}\n" "$description" "$actual"
      PASS=$((PASS + 1))
    else
      printf "  ${RED}FAIL${RESET} %s\n" "$description"
      printf "       expected: %s\n" "$expected_cmd"
      printf "       actual:   %s\n" "$actual"
      FAIL=$((FAIL + 1))
    fi
  fi
}

echo "============================================"
echo "  RTK Rewrite Hook Test Suite"
echo "============================================"
echo ""

# ---- SECTION 1: Existing patterns (regression tests) ----
echo "--- Existing patterns (regression) ---"
test_rewrite "git status" \
  "git status" \
  "rtk git status"

test_rewrite "git log --oneline -10" \
  "git log --oneline -10" \
  "rtk git log --oneline -10"

test_rewrite "git diff HEAD" \
  "git diff HEAD" \
  "rtk git diff HEAD"

test_rewrite "git show abc123" \
  "git show abc123" \
  "rtk git show abc123"

test_rewrite "git add ." \
  "git add ." \
  ""

test_rewrite "git commit -m msg (mutating guarded default)" \
  "git commit -m 'msg'" \
  ""

test_rewrite "git push (mutating guarded default)" \
  "git push" \
  ""

test_rewrite "git checkout (mutating guarded default)" \
  "git checkout feat/gain-project-scope" \
  ""

test_rewrite "git cherry-pick (mutating guarded default)" \
  "git cherry-pick 3d08e6c" \
  ""

test_rewrite "git merge (mutating guarded default)" \
  "git merge origin/master" \
  ""

test_rewrite "git rebase (mutating guarded default)" \
  "git rebase upstream/master" \
  ""

test_rewrite "git rm (mutating guarded default)" \
  "git rm --cached .grepai/config.yaml" \
  ""

test_rewrite "git remote -v" \
  "git remote -v" \
  "rtk git remote -v"

test_rewrite "git merge-base" \
  "git merge-base HEAD origin/master" \
  "rtk git merge-base HEAD origin/master"

test_rewrite "git rev-parse" \
  "git rev-parse HEAD" \
  "rtk git rev-parse HEAD"

test_rewrite "git ls-files" \
  "git ls-files --cached" \
  "rtk git ls-files --cached"

test_rewrite "git -C status" \
  "git -C /Users/andrew/Programming/rtk status -s" \
  "rtk git -C /Users/andrew/Programming/rtk status -s"

test_rewrite "gh pr list" \
  "gh pr list" \
  "rtk gh pr list"

test_rewrite "npx playwright test" \
  "npx playwright test" \
  "rtk playwright test"

test_rewrite "ls -la" \
  "ls -la" \
  "rtk ls -la"

test_rewrite "curl -s https://example.com" \
  "curl -s https://example.com" \
  "rtk curl -s https://example.com"

test_rewrite "cat package.json" \
  "cat package.json" \
  "rtk read package.json"

test_rewrite "cat with flag -e (no rewrite, unsafe)" \
  "cat -e src/main.rs" \
  ""

test_rewrite "cat multiple files (no rewrite, multi-file)" \
  "cat src/a.rs src/b.rs" \
  ""

test_rewrite "cat -n file (no rewrite, has flag)" \
  "cat -n src/main.rs" \
  ""

test_rewrite "head -20 package.json" \
  "head -20 package.json" \
  "rtk read package.json --max-lines 20"

test_rewrite "grep -rn pattern src/" \
  "grep -rn pattern src/" \
  "rtk grep -rn pattern src/"

test_rewrite "rg pattern src/" \
  "rg pattern src/" \
  "rtk grep pattern src/"

test_rewrite "grepai search rewrite" \
  "grepai search \"SharedDefaults App Group\"" \
  "rtk rgai \"SharedDefaults App Group\""

test_rewrite "grepai absolute path rewrite" \
  "/Users/andrew/.local/bin/grepai search \"SharedDefaults App Group\"" \
  "rtk rgai \"SharedDefaults App Group\""

test_rewrite "rgai direct rewrite" \
  "rgai token traces" \
  "rtk rgai token traces"

test_rewrite "cargo test" \
  "cargo test" \
  "rtk cargo test"

test_rewrite "cargo run passthrough" \
  "cargo run -- rgai --builtin \"token trace\"" \
  "rtk cargo run -- rgai --builtin \"token trace\""

test_rewrite "cargo absolute path rewrite" \
  "/Users/andrew/.cargo/bin/cargo test -q" \
  "rtk cargo test -q"

test_rewrite "npx prisma migrate" \
  "npx prisma migrate" \
  "rtk prisma migrate"

test_rewrite "python -m pytest" \
  "python -m pytest benchmarks/tests/test_baseline.py" \
  "rtk pytest benchmarks/tests/test_baseline.py"

test_rewrite "python3 -m pytest" \
  "python3 -m pytest benchmarks/tests/test_baseline.py" \
  "rtk pytest benchmarks/tests/test_baseline.py"

test_rewrite "go build rewrite" \
  "go build ./internal/domain/game/services/..." \
  "rtk go build ./internal/domain/game/services/..."

test_rewrite "go test rewrite" \
  "go test ./internal/domain/character/..." \
  "rtk go test ./internal/domain/character/..."

test_rewrite "go vet rewrite" \
  "go vet ./internal/domain/game/services/..." \
  "rtk go vet ./internal/domain/game/services/..."

echo ""

# ---- SECTION 1.5: Mutating rewrites (opt-in) ----
echo "--- Mutating rewrites (RTK_REWRITE_MUTATING=1) ---"
test_rewrite_with_env "git add . with mutating enabled" \
  "git add ." \
  "rtk git add ." \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git commit with mutating enabled" \
  "git commit -m 'msg'" \
  "rtk git commit -m 'msg'" \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git checkout with mutating enabled" \
  "git checkout feat/gain-project-scope" \
  "rtk git checkout feat/gain-project-scope" \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git cherry-pick with mutating enabled" \
  "git cherry-pick 3d08e6c" \
  "rtk git cherry-pick 3d08e6c" \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git merge with mutating enabled" \
  "git merge origin/master" \
  "rtk git merge origin/master" \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git rebase with mutating enabled" \
  "git rebase upstream/master" \
  "rtk git rebase upstream/master" \
  "RTK_REWRITE_MUTATING=1"

test_rewrite_with_env "git rm with mutating enabled" \
  "git rm --cached .grepai/config.yaml" \
  "rtk git rm --cached .grepai/config.yaml" \
  "RTK_REWRITE_MUTATING=1"

echo ""

# ---- SECTION 2: Env var prefix handling (THE BIG FIX) ----
echo "--- Env var prefix handling (new) ---"
test_rewrite "env + playwright" \
  "TEST_SESSION_ID=2 npx playwright test --config=foo" \
  "TEST_SESSION_ID=2 rtk playwright test --config=foo"

test_rewrite "env + git status" \
  "GIT_PAGER=cat git status" \
  "GIT_PAGER=cat rtk git status"

test_rewrite "env + git log" \
  "GIT_PAGER=cat git log --oneline -10" \
  "GIT_PAGER=cat rtk git log --oneline -10"

test_rewrite "multi env + vitest" \
  "NODE_ENV=test CI=1 npx vitest run" \
  "NODE_ENV=test CI=1 rtk vitest run"

test_rewrite "env + ls" \
  "LANG=C ls -la" \
  "LANG=C rtk ls -la"

test_rewrite "env + npm run" \
  "NODE_ENV=test npm run test:e2e" \
  "NODE_ENV=test rtk npm test:e2e"

test_rewrite "env + docker compose" \
  "COMPOSE_PROJECT_NAME=test docker compose up -d" \
  "COMPOSE_PROJECT_NAME=test rtk docker compose up -d"

echo ""

# ---- SECTION 2.5: Comment-prefix stripping ----
echo "--- Comment-prefix stripping (bug fix) ---"
test_rewrite "# comment then cat" \
  $'# explain what we do\ncat src/main.rs' \
  "rtk read src/main.rs"

test_rewrite "# comment then git status" \
  $'# checking status\ngit status' \
  "rtk git status"

test_rewrite "# comment then cargo test" \
  $'# run tests\ncargo test' \
  "rtk cargo test"

test_rewrite "# already rtk after comments (no double rewrite)" \
  $'# already good\nrtk cargo test' \
  ""

echo ""

# ---- SECTION 3: New patterns ----
echo "--- New patterns ---"
test_rewrite "npm run test:e2e" \
  "npm run test:e2e" \
  "rtk npm test:e2e"

test_rewrite "npm run build" \
  "npm run build" \
  "rtk npm build"

test_rewrite "npm test" \
  "npm test" \
  "rtk npm test"

test_rewrite "bun run typecheck" \
  "bun run typecheck" \
  "rtk bun run typecheck"

test_rewrite "bun direct script" \
  "bun packages/server/src/index.ts 2>&1" \
  "rtk bun packages/server/src/index.ts 2>&1"

test_rewrite "bun --version" \
  "bun --version 2>/dev/null" \
  "rtk bun --version 2>/dev/null"

test_rewrite "vue-tsc -b" \
  "vue-tsc -b" \
  "rtk npx vue-tsc -b"

test_rewrite "npx vue-tsc --noEmit" \
  "npx vue-tsc --noEmit" \
  "rtk npx vue-tsc --noEmit"

test_rewrite "vue tsc shorthand" \
  "vue tsc --noEmit" \
  "rtk npx vue-tsc --noEmit"

test_rewrite "docker compose up -d" \
  "docker compose up -d" \
  "rtk docker compose up -d"

test_rewrite "docker compose logs postgrest" \
  "docker compose logs postgrest" \
  "rtk docker compose logs postgrest"

test_rewrite "docker compose down" \
  "docker compose down" \
  "rtk docker compose down"

test_rewrite "docker run --rm postgres" \
  "docker run --rm postgres" \
  "rtk docker run --rm postgres"

test_rewrite "docker exec -it db psql" \
  "docker exec -it db psql" \
  "rtk docker exec -it db psql"

test_rewrite "find rewrite" \
  "find . -name '*.ts'" \
  "rtk find . -name '*.ts'"

test_rewrite "tree rewrite" \
  "tree src/" \
  "rtk tree src/"

test_rewrite "wget rewrite" \
  "wget https://example.com/file" \
  "rtk wget https://example.com/file"

test_rewrite "gh api repos/owner/repo" \
  "gh api repos/owner/repo" \
  "rtk gh api repos/owner/repo"

test_rewrite "gh release list" \
  "gh release list" \
  "rtk gh release list"

test_rewrite "kubectl describe pod foo" \
  "kubectl describe pod foo" \
  "rtk kubectl describe pod foo"

test_rewrite "kubectl apply -f deploy.yaml" \
  "kubectl apply -f deploy.yaml" \
  "rtk kubectl apply -f deploy.yaml"

test_rewrite "write replace alias" \
  "write replace src/main.rs --from old --to new" \
  "rtk write replace src/main.rs --from old --to new"

test_rewrite "sed -i single occurrence" \
  "sed -i 's/old/new/' file.txt" \
  "rtk write replace file.txt --from 'old' --to 'new' --retry 3"

test_rewrite "sed -i global occurrence" \
  "sed -i 's/old/new/g' file.txt" \
  "rtk write replace file.txt --from 'old' --to 'new' --retry 3 --all"

test_rewrite "perl -pi replacement" \
  "perl -pi -e 's/old/new/g' file.txt" \
  "rtk write replace file.txt --from 'old' --to 'new' --retry 3 --all"

echo ""

# ---- SECTION 4: Vitest edge case (fixed double "run" bug) ----
echo "--- Vitest run dedup ---"
test_rewrite "vitest (no args)" \
  "vitest" \
  "rtk vitest run"

test_rewrite "vitest run (no double run)" \
  "vitest run" \
  "rtk vitest run"

test_rewrite "vitest run --reporter" \
  "vitest run --reporter=verbose" \
  "rtk vitest run --reporter=verbose"

test_rewrite "npx vitest run" \
  "npx vitest run" \
  "rtk vitest run"

test_rewrite "pnpm vitest run --coverage" \
  "pnpm vitest run --coverage" \
  "rtk vitest run --coverage"

echo ""

# ---- SECTION 5: Should NOT rewrite ----
echo "--- Should NOT rewrite ---"
test_rewrite "already rtk" \
  "rtk git status" \
  ""

test_rewrite "heredoc" \
  "cat <<'EOF'
hello
EOF" \
  ""

test_rewrite "echo (no pattern)" \
  "echo hello world" \
  ""

test_rewrite "cd (no pattern)" \
  "cd /tmp" \
  ""

test_rewrite "mkdir (no pattern)" \
  "mkdir -p foo/bar" \
  ""

test_rewrite "python3 (no pattern)" \
  "python3 script.py" \
  ""

test_rewrite "pip absolute path rewrite" \
  "/Users/andrew/anaconda3/bin/pip install requests" \
  "rtk pip install requests"

test_rewrite "bun install (no safe rewrite)" \
  "bun install" \
  ""

test_rewrite "node (no pattern)" \
  "node -e 'console.log(1)'" \
  ""

test_rewrite "tail (no safe rewrite)" \
  "tail -20 package.json" \
  ""

echo ""

# ---- SECTION 6: Audit logging ----
echo "--- Audit logging (RTK_HOOK_AUDIT=1) ---"

AUDIT_TMPDIR=$(mktemp -d)
trap "rm -rf $AUDIT_TMPDIR" EXIT

test_audit_log() {
  local description="$1"
  local input_cmd="$2"
  local expected_action="$3"
  TOTAL=$((TOTAL + 1))

  # Clean log
  rm -f "$AUDIT_TMPDIR/hook-audit.log"

  local input_json
  input_json=$(jq -n --arg cmd "$input_cmd" '{"tool_name":"Bash","tool_input":{"command":$cmd}}')
  echo "$input_json" | RTK_HOOK_AUDIT=1 RTK_AUDIT_DIR="$AUDIT_TMPDIR" bash "$HOOK" 2>/dev/null || true

  if [ ! -f "$AUDIT_TMPDIR/hook-audit.log" ]; then
    printf "  ${RED}FAIL${RESET} %s (no log file created)\n" "$description"
    FAIL=$((FAIL + 1))
    return
  fi

  local log_line
  log_line=$(head -1 "$AUDIT_TMPDIR/hook-audit.log")
  local actual_action
  actual_action=$(echo "$log_line" | cut -d'|' -f2 | tr -d ' ')

  if [ "$actual_action" = "$expected_action" ]; then
    printf "  ${GREEN}PASS${RESET} %s ${DIM}→ %s${RESET}\n" "$description" "$actual_action"
    PASS=$((PASS + 1))
  else
    printf "  ${RED}FAIL${RESET} %s\n" "$description"
    printf "       expected action: %s\n" "$expected_action"
    printf "       actual action:   %s\n" "$actual_action"
    printf "       log line:        %s\n" "$log_line"
    FAIL=$((FAIL + 1))
  fi
}

test_audit_log "audit: rewrite git status" \
  "git status" \
  "rewrite"

test_audit_log "audit: skip already_rtk" \
  "rtk git status" \
  "skip:already_rtk"

test_audit_log "audit: skip heredoc" \
  "cat <<'EOF'
hello
EOF" \
  "skip:heredoc"

test_audit_log "audit: skip no_match" \
  "echo hello world" \
  "skip:no_match"

test_audit_log "audit: rewrite cargo test" \
  "cargo test" \
  "rewrite"

test_audit_log "audit: mutating guard for git add" \
  "git add ." \
  "skip:mutating_guard"

# Test log format (5 pipe-separated fields: timestamp | action | class=... | original | rewritten)
rm -f "$AUDIT_TMPDIR/hook-audit.log"
input_json=$(jq -n --arg cmd "git status" '{"tool_name":"Bash","tool_input":{"command":$cmd}}')
echo "$input_json" | RTK_HOOK_AUDIT=1 RTK_AUDIT_DIR="$AUDIT_TMPDIR" bash "$HOOK" 2>/dev/null || true
TOTAL=$((TOTAL + 1))
log_line=$(cat "$AUDIT_TMPDIR/hook-audit.log" 2>/dev/null || echo "")
field_count=$(echo "$log_line" | awk -F' \\| ' '{print NF}')
if [ "$field_count" = "5" ]; then
  printf "  ${GREEN}PASS${RESET} audit: log format has 5 fields ${DIM}→ %s${RESET}\n" "$log_line"
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${RESET} audit: log format (expected 5 fields, got %s)\n" "$field_count"
  printf "       log line: %s\n" "$log_line"
  FAIL=$((FAIL + 1))
fi

# Test no log when RTK_HOOK_AUDIT is unset
rm -f "$AUDIT_TMPDIR/hook-audit.log"
input_json=$(jq -n --arg cmd "git status" '{"tool_name":"Bash","tool_input":{"command":$cmd}}')
echo "$input_json" | RTK_AUDIT_DIR="$AUDIT_TMPDIR" bash "$HOOK" 2>/dev/null || true
TOTAL=$((TOTAL + 1))
if [ ! -f "$AUDIT_TMPDIR/hook-audit.log" ]; then
  printf "  ${GREEN}PASS${RESET} audit: no log when RTK_HOOK_AUDIT unset\n"
  PASS=$((PASS + 1))
else
  printf "  ${RED}FAIL${RESET} audit: log created when RTK_HOOK_AUDIT unset\n"
  FAIL=$((FAIL + 1))
fi

echo ""

# ---- SUMMARY ----
echo "============================================"
if [ $FAIL -eq 0 ]; then
  printf "  ${GREEN}ALL $TOTAL TESTS PASSED${RESET}\n"
else
  printf "  ${RED}$FAIL FAILED${RESET} / $TOTAL total ($PASS passed)\n"
fi
echo "============================================"

exit $FAIL
