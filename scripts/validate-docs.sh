#!/usr/bin/env bash
set -euo pipefail

echo "Validating RTK documentation consistency..."

# Count only top-level module declarations (`mod name;`), not inline test modules.
main_modules="$(grep -Ec '^mod [a-zA-Z0-9_]+;' src/main.rs || true)"
echo "main.rs modules: ${main_modules}"

if [[ -f "scripts/sync-architecture-modules.sh" ]]; then
  bash scripts/sync-architecture-modules.sh ARCHITECTURE.md
fi

if [[ -f "ARCHITECTURE.md" ]]; then
  arch_modules="$(grep 'Total:.*modules' ARCHITECTURE.md | grep -o '[0-9]\+' | head -1 || true)"
  if [[ -n "${arch_modules}" ]]; then
    echo "ARCHITECTURE.md modules: ${arch_modules}"
    if [[ "${main_modules}" != "${arch_modules}" ]]; then
      echo "WARN: module count mismatch (main.rs=${main_modules}, ARCHITECTURE.md=${arch_modules})"
      echo "WARN: run scripts/sync-architecture-modules.sh and commit ARCHITECTURE.md update."
    fi
  else
    echo "WARN: could not parse module count from ARCHITECTURE.md"
  fi
fi

if ! git diff --quiet -- ARCHITECTURE.md 2>/dev/null; then
  echo "WARN: ARCHITECTURE.md was auto-synced during validation. Commit this file to keep docs current."
fi

for doc in README.md CLAUDE.md; do
  if [[ ! -f "${doc}" ]]; then
    echo "ERROR: missing required documentation file: ${doc}"
    exit 1
  fi
done

commands=(ruff pytest pip go golangci)
for cmd in "${commands[@]}"; do
  if ! grep -q "${cmd}" README.md; then
    echo "ERROR: README.md missing command mention: ${cmd}"
    exit 1
  fi
  if ! grep -q "${cmd}" CLAUDE.md; then
    echo "ERROR: CLAUDE.md missing command mention: ${cmd}"
    exit 1
  fi
done
echo "README/CLAUDE command coverage: OK"

for hook in .claude/hooks/rtk-rewrite.sh hooks/rtk-rewrite.sh; do
  if [[ ! -f "${hook}" ]]; then
    echo "ERROR: missing hook file: ${hook}"
    exit 1
  fi
  for cmd in ruff pytest pip "go " golangci; do
    if ! grep -q "${cmd}" "${hook}"; then
      echo "ERROR: ${hook} missing rewrite coverage for: ${cmd}"
      exit 1
    fi
  done
done
echo "Hook coverage: OK"

echo "Documentation validation passed"
