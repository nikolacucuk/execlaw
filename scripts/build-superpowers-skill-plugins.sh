#!/usr/bin/env bash
set -euo pipefail

# Build execlaw plugin ZIPs from Superpowers skill directories.
# Produces:
#   dist/superpowers-shared-<version>.zip
#   dist/superpowers-user-<version>.zip (optional)

SHARED_SKILLS_ROOT="${SHARED_SKILLS_ROOT:-$HOME/.config/superpowers/skills}"
USER_SKILLS_ROOT="${USER_SKILLS_ROOT:-$HOME/.execlaw/skills/user}"
USER_SKILL_NAMESPACE="${USER_SKILL_NAMESPACE:-djenka}"
DIST_DIR="${DIST_DIR:-dist}"
SHARED_PLUGIN_ID="${SHARED_PLUGIN_ID:-superpowers-shared}"
USER_PLUGIN_ID="${USER_PLUGIN_ID:-superpowers-user}"
SKIP_USER="${SKIP_USER:-0}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

escape_toml() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

plugin_version() {
  local root="$1"
  local d
  d="$(date +%Y.%m.%d)"
  if [[ -d "$root/.git" ]]; then
    local sha
    sha="$(git -C "$root" rev-parse --short HEAD 2>/dev/null || true)"
    if [[ -n "$sha" ]]; then
      printf '%s+%s' "$d" "$sha"
      return
    fi
  fi
  printf '%s' "$d"
}

skill_frontmatter_description() {
  local skill_file="$1"
  awk '
    BEGIN { in_fm=0 }
    NR==1 && $0=="---" { in_fm=1; next }
    in_fm && $0=="---" { exit }
    in_fm {
      if ($0 ~ /^description:[[:space:]]*/) {
        sub(/^description:[[:space:]]*/, "", $0)
        gsub(/^"|"$/, "", $0)
        gsub(/^'"'"'|'"'"'$/, "", $0)
        print $0
        exit
      }
    }
  ' "$skill_file"
}

collect_skills() {
  local root="$1"
  if [[ ! -d "$root" ]]; then
    echo "skills root missing: $root" >&2
    return 1
  fi

  find "$root" -type f -name SKILL.md | sort
}

write_manifest() {
  local manifest_path="$1"
  local plugin_id="$2"
  local plugin_name="$3"
  local version="$4"
  local description="$5"
  local list_file="$6"

  {
    echo "[plugin]"
    echo "id = \"$(escape_toml "$plugin_id")\""
    echo "name = \"$(escape_toml "$plugin_name")\""
    echo "version = \"$(escape_toml "$version")\""
    echo "description = \"$(escape_toml "$description")\""
    echo "author = \"execlaw integration\""
    echo "license = \"MIT\""
    echo

    while IFS=$'\t' read -r rel_dir local_name desc; do
      [[ -z "$rel_dir" ]] && continue
      echo "[[skills]]"
      echo "name = \"$(escape_toml "$local_name")\""
      echo "description = \"$(escape_toml "$desc")\""
      echo "entry = \"skills/$rel_dir/SKILL.md\""
      echo "tags = [\"superpowers\"]"
      echo
    done < "$list_file"
  } > "$manifest_path"
}

build_plugin_zip() {
  local source_root="$1"
  local plugin_id="$2"
  local plugin_name="$3"
  local plugin_description="$4"
  local name_prefix="$5"

  local version
  version="$(plugin_version "$source_root")"

  local stage
  stage="$(mktemp -d "${TMPDIR:-/tmp}/execlaw-superpowers.XXXXXX")"
  mkdir -p "$stage/skills"

  local list_file
  list_file="$stage/skills.list.tsv"
  : > "$list_file"

  while IFS= read -r skill_file; do
    local skill_dir rel_dir local_name desc
    skill_dir="$(dirname "$skill_file")"
    rel_dir="${skill_dir#$source_root/}"

    local_name="${rel_dir//\//-}"
    if [[ -n "$name_prefix" ]]; then
      local_name="$name_prefix$local_name"
    fi

    desc="$(skill_frontmatter_description "$skill_file")"
    if [[ -z "$desc" ]]; then
      desc="Superpowers skill imported from $rel_dir"
    fi

    mkdir -p "$stage/skills/$rel_dir"
    cp -R "$skill_dir"/. "$stage/skills/$rel_dir/"

    printf '%s\t%s\t%s\n' "$rel_dir" "$local_name" "$desc" >> "$list_file"
  done < <(collect_skills "$source_root")

  if [[ ! -s "$list_file" ]]; then
    echo "no skills discovered for $plugin_id; skipping" >&2
    rm -rf "$stage"
    return 0
  fi

  write_manifest "$stage/plugin.toml" "$plugin_id" "$plugin_name" "$version" "$plugin_description" "$list_file"

  mkdir -p "$DIST_DIR"
  local out="$DIST_DIR/$plugin_id-$version.zip"
  rm -f "$out"

  (
    cd "$stage"
    zip -qr "$REPO_ROOT/$out" .
  )

  (
    cd "$DIST_DIR"
    shasum -a 256 "$(basename "$out")" > "$(basename "$out").sha256"
  )

  echo "$out"
  rm -rf "$stage"
}

shared_zip="$(build_plugin_zip "$SHARED_SKILLS_ROOT" "$SHARED_PLUGIN_ID" "Superpowers Shared Skills" "Shared Superpowers skills imported into execlaw." "")"
user_zip=""

if [[ "$SKIP_USER" != "1" && -d "$USER_SKILLS_ROOT" ]]; then
  user_zip="$(build_plugin_zip "$USER_SKILLS_ROOT" "$USER_PLUGIN_ID" "Superpowers User Skills" "User-scoped Superpowers overlays for one operator." "$USER_SKILL_NAMESPACE-")"
fi

echo
echo "Superpowers skill plugin build complete"
[[ -n "$shared_zip" ]] && echo "  Shared: $shared_zip"
[[ -n "$user_zip" ]] && echo "  User:   $user_zip"
echo "Install ZIPs via Settings -> Plugins -> Install"
