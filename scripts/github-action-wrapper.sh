#!/usr/bin/env bash
set -euo pipefail

hash_file() {
  local file_path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file_path" | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file_path" | awk '{print $1}'
    return
  fi

  if command -v python3 >/dev/null 2>&1; then
    python3 - "$file_path" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
    return
  fi

  if command -v python >/dev/null 2>&1; then
    python - "$file_path" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
    return
  fi

  echo "unable to locate a SHA-256 implementation" >&2
  exit 1
}

hash_string() {
  local value="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$value" | sha256sum | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "$value" | shasum -a 256 | awk '{print $1}'
    return
  fi

  if command -v python3 >/dev/null 2>&1; then
    python3 - "$value" <<'PY'
import hashlib
import sys

print(hashlib.sha256(sys.argv[1].encode()).hexdigest())
PY
    return
  fi

  if command -v python >/dev/null 2>&1; then
    python - "$value" <<'PY'
import hashlib
import sys

print(hashlib.sha256(sys.argv[1].encode()).hexdigest())
PY
    return
  fi

  echo "unable to locate a SHA-256 implementation" >&2
  exit 1
}

sanitize_key_part() {
  printf '%s' "${1:-}" | tr -c 'A-Za-z0-9._-' '-'
}

emit_output() {
  printf '%s=%s\n' "$1" "$2"
}

resolve_repository_path() {
  local path="$1"

  case "$path" in
    /*)
      printf '%s\n' "$path"
      ;;
    ./*)
      printf '%s/%s\n' "${GITHUB_WORKSPACE%/}" "${path#./}"
      ;;
    *)
      printf '%s/%s\n' "${GITHUB_WORKSPACE%/}" "$path"
      ;;
  esac
}

require_env() {
  local key="$1"
  if [ -z "${!key:-}" ]; then
    echo "required environment variable \`$key\` is not set" >&2
    exit 1
  fi
}

resolve_context() {
  require_env GITHUB_ACTION_PATH
  require_env RUNNER_TEMP

  local repo_id
  repo_id="$(sanitize_key_part "${GITHUB_REPOSITORY_ID:-unknown-repository}")"
  local case_material
  case_material="$(printf 'working-directory=%s\nargs=%s' "${INPUT_WORKING_DIRECTORY:-.}" "${INPUT_ARGS:-}")"
  local case_seed
  case_seed="$(hash_string "$case_material")"
  case_seed="${case_seed%% *}"
  case_seed="${case_seed:0:16}"
  local cache_namespace="${repo_id}-${case_seed}"

  local cache_dir
  if [ -n "${INPUT_CACHE_DIR:-}" ]; then
    cache_dir="${INPUT_CACHE_DIR}"
  else
    cache_dir="${RUNNER_TEMP%/}/kalos-cache/${cache_namespace}"
  fi

  local install_root="${RUNNER_TEMP%/}/kalos-tool"
  local binary_suffix=""
  if [ "${RUNNER_OS:-}" = "Windows" ]; then
    binary_suffix=".exe"
  fi
  local kalos_bin="${install_root}/bin/kalos${binary_suffix}"

  local bundle_manifest_file="${GITHUB_ACTION_PATH}/src/adapters/tool_cache/managed_bundle.rs"
  local bundle_seed
  bundle_seed="$(hash_file "$bundle_manifest_file")"
  bundle_seed="${bundle_seed%% *}"
  bundle_seed="${bundle_seed:0:16}"

  local runner_os
  runner_os="$(sanitize_key_part "${RUNNER_OS:-unknown-os}")"
  local runner_arch
  runner_arch="$(sanitize_key_part "${RUNNER_ARCH:-unknown-arch}")"
  local ref_name
  ref_name="$(sanitize_key_part "${GITHUB_REF_NAME:-detached-head}")"
  local scope
  scope="$(sanitize_key_part "${INPUT_BASELINE_CACHE_SCOPE:-default}")"
  local sha
  sha="$(sanitize_key_part "${GITHUB_SHA:-unknown-sha}")"

  local bundle_cache_key
  local baseline_restore_prefix
  if [ -n "${INPUT_CACHE_DIR:-}" ]; then
    bundle_cache_key="kalos-bundle-${runner_os}-${runner_arch}-${bundle_seed}"
    baseline_restore_prefix="kalos-baseline-${runner_os}-${runner_arch}-${repo_id}-${scope}-${ref_name}-"
  else
    bundle_cache_key="kalos-bundle-${runner_os}-${runner_arch}-${repo_id}-${case_seed}-${bundle_seed}"
    baseline_restore_prefix="kalos-baseline-${runner_os}-${runner_arch}-${repo_id}-${scope}-${case_seed}-${ref_name}-"
  fi
  local baseline_cache_key="${baseline_restore_prefix}${sha}"
  local sarif_file=""
  local sarif_file_abs=""
  if [ "${INPUT_UPLOAD_SARIF:-false}" = "true" ]; then
    require_env GITHUB_WORKSPACE
    require_env INPUT_SARIF_FILE
    sarif_file="$INPUT_SARIF_FILE"
    sarif_file_abs="$(resolve_repository_path "$INPUT_SARIF_FILE")"
  fi

  emit_output cache_dir "$cache_dir"
  emit_output install_root "$install_root"
  emit_output kalos_bin "$kalos_bin"
  emit_output bundle_cache_key "$bundle_cache_key"
  emit_output baseline_restore_prefix "$baseline_restore_prefix"
  emit_output baseline_cache_key "$baseline_cache_key"
  emit_output sarif_file "$sarif_file"
  emit_output sarif_file_abs "$sarif_file_abs"
}

mktemp_dir() {
  local temp_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
  mkdir -p "$temp_root"
  mktemp -d "${temp_root%/}/kalos-prewarm.XXXXXX"
}

prewarm() {
  require_env KALOS_BIN
  require_env KALOS_CACHE_DIR

  local workspace
  workspace="$(mktemp_dir)"
  trap 'rm -rf "'"$workspace"'"' EXIT

  mkdir -p "$workspace/src"
  cat >"$workspace/Cargo.toml" <<'EOF'
[package]
name = "kalos-action-prewarm"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
EOF
  cat >"$workspace/src/lib.rs" <<'EOF'
pub fn kalos_action_prewarm() -> i32 {
    1
}
EOF

  (
    cd "$workspace"
    "$KALOS_BIN" check --level project --format json >"$workspace/prewarm-report.json"
  )
}

args_contain_conflicting_flag() {
  local pending_value=0
  local arg

  for arg in "$@"; do
    if [ "$pending_value" -eq 1 ]; then
      return 0
    fi

    case "$arg" in
      --format)
        pending_value=1
        ;;
      --format=*)
        return 0
        ;;
      --output|-o)
        pending_value=1
        ;;
      -o?*)
        return 0
        ;;
      --output=*)
        return 0
        ;;
    esac
  done

  return 1
}

git_ref_exists() {
  git rev-parse --verify --quiet "$1^{commit}" >/dev/null 2>&1
}

remote_default_branch_ref() {
  local ref

  ref="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
  if [ -n "$ref" ] && git_ref_exists "$ref"; then
    printf '%s\n' "$ref"
    return 0
  fi

  for ref in origin/main origin/master; do
    if git_ref_exists "$ref"; then
      printf '%s\n' "$ref"
      return 0
    fi
  done

  local remote_refs=()
  while IFS= read -r ref; do
    if [ -n "$ref" ] && git_ref_exists "$ref"; then
      remote_refs+=("$ref")
    fi
  done < <(git for-each-ref --format='%(refname:short)' refs/remotes/origin 2>/dev/null | grep -v '^origin/HEAD$' || true)

  if [ "${#remote_refs[@]}" -eq 1 ]; then
    printf '%s\n' "${remote_refs[0]}"
    return 0
  fi

  return 1
}

resolve_diff_ref() {
  local ref="$1"
  local fallback_ref

  if [ "$ref" != "origin/develop" ]; then
    printf '%s\n' "$ref"
    return
  fi

  if git_ref_exists "$ref"; then
    printf '%s\n' "$ref"
    return
  fi

  if fallback_ref="$(remote_default_branch_ref)"; then
    echo "notice: --diff origin/develop is unavailable; using remote default branch ${fallback_ref}" >&2
    printf '%s\n' "$fallback_ref"
    return
  fi

  echo "notice: --diff origin/develop is unavailable and no remote default branch ref could be resolved; forwarding origin/develop unchanged" >&2
  printf '%s\n' "$ref"
}

resolve_diff_args() {
  local resolved=()
  local pending_diff=0
  local arg

  for arg in "$@"; do
    if [ "$pending_diff" -eq 1 ]; then
      resolved+=("$(resolve_diff_ref "$arg")")
      pending_diff=0
      continue
    fi

    case "$arg" in
      --diff)
        resolved+=("$arg")
        pending_diff=1
        ;;
      --diff=*)
        resolved+=("--diff=$(resolve_diff_ref "${arg#--diff=}")")
        ;;
      *)
        resolved+=("$arg")
        ;;
    esac
  done

  printf '%s\n' "${resolved[@]}"
}

run_check() {
  require_env KALOS_BIN

  local args=()
  if [ -n "${KALOS_ACTION_ARGS:-}" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      if [ -n "$line" ]; then
        args+=("$line")
      fi
    done <<EOF
${KALOS_ACTION_ARGS}
EOF
  fi

  if [ "${#args[@]}" -gt 0 ] && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    local resolved_args=()
    local resolved_arg
    while IFS= read -r resolved_arg; do
      resolved_args+=("$resolved_arg")
    done < <(resolve_diff_args "${args[@]}")
    args=("${resolved_args[@]}")
  fi

  if [ -n "${KALOS_ACTION_SARIF_FILE:-}" ]; then
    if args_contain_conflicting_flag "${args[@]}"; then
      echo "KALOS_ACTION_SARIF_FILE is set; omit --format/--output from args and let the wrapper manage SARIF output" >&2
      exit 1
    fi

    "$KALOS_BIN" check --format sarif --output "$KALOS_ACTION_SARIF_FILE" "${args[@]}"
  elif [ "${#args[@]}" -eq 0 ]; then
    "$KALOS_BIN" check
  else
    "$KALOS_BIN" check "${args[@]}"
  fi
}

main() {
  if [ "$#" -ne 1 ]; then
    echo "usage: $0 <resolve-context|prewarm|run-check>" >&2
    exit 1
  fi

  case "$1" in
    resolve-context)
      resolve_context
      ;;
    prewarm)
      prewarm
      ;;
    run-check)
      run_check
      ;;
    *)
      echo "unknown command: $1" >&2
      exit 1
      ;;
  esac
}

main "$@"
