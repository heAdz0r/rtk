#!/usr/bin/env bash
set -euo pipefail

echo "Validating RTK documentation consistency..."

main_modules="$(grep -c '^mod ' src/main.rs || true)"
echo "main.rs modules: ${main_modules}"

if [[ -f "ARCHITECTURE.md" ]]; then
  arch_modules="$(grep 'Total:.*modules' ARCHITECTURE.md | grep -o '[0-9]\+' | head -1 || true)"
  if [[ -n "${arch_modules}" ]]; then
    echo "ARCHITECTURE.md modules: ${arch_modules}"
    if [[ "${main_modules}" != "${arch_modules}" ]]; then
      echo "ERROR: module count mismatch (main.rs=${main_modules}, ARCHITECTURE.md=${arch_modules})"
      exit 1
    fi
  else
    echo "WARN: could not parse module count from ARCHITECTURE.md"
  fi
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
