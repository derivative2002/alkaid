#!/usr/bin/env bash
# Local mirror of the CI checks in .github/workflows/ci.yml.
#
# The point of this script is that a contributor can run one command on this
# machine and get the same pass/fail verdict the pipeline would give, without
# pushing a branch first. Every gate below exists in the GitHub workflow too,
# in the same order, with the same flags; .gitlab-ci.yml mirrors both.
#
# The Rust toolchain lives on the persistent volume behind the g2 shims in
# /volume/yjiao/tools/bin, so the shim directory has to come first on PATH.
# CC/AR/RANLIB point at zig, which is what the shims wrap.
#
# Usage: bash scripts/check-local.sh [gate ...]
#   With no arguments every gate runs. Naming gates runs only those, which is
#   what you want while iterating: bash scripts/check-local.sh fmt clippy
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

export PATH=/volume/yjiao/tools/bin:/home/ray/anaconda3/bin:$PATH
export CC=/volume/yjiao/tools/bin/cc
export AR=/volume/yjiao/tools/bin/ar
export RANLIB=/volume/yjiao/tools/bin/ranlib

# Same workflow-level env as the GitHub workflow. RUSTFLAGS applies -D warnings
# to every rustc invocation, so a warning fails test and build as well.
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"

ALL_GATES=(fmt clippy test build manifest audit)

# A named subset, or everything.
if (($#)); then
  GATES=("$@")
  for gate in "${GATES[@]}"; do
    case " ${ALL_GATES[*]} " in
      *" $gate "*) ;;
      *) printf 'unknown gate: %s (known: %s)\n' "$gate" "${ALL_GATES[*]}" >&2; exit 2 ;;
    esac
  done
else
  GATES=("${ALL_GATES[@]}")
fi

# Gates that were skipped rather than run, reported at the end so a skip is
# never mistaken for a pass.
SKIPPED=()

# run_gate <name>
#
# Runs one gate's command and prints the verdict, returning the gate's own exit
# status. Output is not filtered: the failure message is the whole value of
# running this locally.
run_gate() {
  local name="$1" status
  printf '\n=== %s ===\n' "$name"
  case "$name" in
    fmt)      printf '+ cargo fmt --all -- --check\n'
              cargo fmt --all -- --check ;;
    clippy)   printf '+ cargo clippy --all-targets --all-features -- -D warnings\n'
              cargo clippy --all-targets --all-features -- -D warnings ;;
    test)     printf '+ cargo test --all-targets --all-features --locked\n'
              cargo test --all-targets --all-features --locked ;;
    build)    printf '+ cargo build --release --locked\n'
              cargo build --release --locked ;;
    manifest) printf '+ python3 (tomllib) herdr-plugin.toml\n'
              validate_manifest ;;
    audit)    printf '+ cargo audit --deny warnings\n'
              validate_audit ;;
  esac
  status=$?
  if ((status == 0)); then
    printf -- '--- %s: PASS\n' "$name"
  elif ((status == 64)); then
    printf -- '--- %s: SKIPPED\n' "$name"
  else
    printf -- '--- %s: FAIL (exit %d)\n' "$name" "$status" >&2
  fi
  return "$status"
}

# The manifest gate is the inline python3 from the GitHub workflow, verbatim.
# tomllib is stdlib only from 3.11; this machine's python3 is 3.10, so fall
# back to tomli, which is the same parser under a different name.
validate_manifest() {
  python3 - <<'PY'
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        sys.exit("no tomllib or tomli available; cannot validate the manifest")

with open("herdr-plugin.toml", "rb") as handle:
    manifest = tomllib.load(handle)
for key in ("id", "name", "version", "min_herdr_version", "description"):
    if not manifest.get(key):
        sys.exit(f"herdr-plugin.toml is missing required key: {key}")
print("herdr-plugin.toml is valid")
PY
}

# cargo-audit is not part of the toolchain image and needs the advisory
# database, so it is only run when it is already installed. A missing binary
# is a skip, not a failure: the audit is a scheduled safety net in CI, not a
# precondition for a local change.
validate_audit() {
  if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "cargo-audit not installed; skipping (CI runs this on a schedule)"
    return 64
  fi
  cargo audit --deny warnings
}

FAILED=()
for gate in "${GATES[@]}"; do
  run_gate "$gate"
  case "$gate:$?" in
    audit:64) SKIPPED+=("$gate") ;;
    *:0) ;;
    *) FAILED+=("$gate") ;;
  esac
done

printf '\n========================================\n'
if ((${#SKIPPED[@]})); then
  printf 'skipped: %s\n' "${SKIPPED[*]}"
fi
if ((${#FAILED[@]})); then
  printf 'failed gates: %s\n' "${FAILED[*]}"
  exit 1
fi
printf 'all gates passed\n'
