#!/usr/bin/env bash
#
# disk-cleanup.sh
#
# Frees disk space by removing:
#   1. Stale Claude worktrees (not associated with running tasks AND no live tmux pane)
#      Includes nested worktrees (worktree inside .claude/worktrees of another worktree).
#   2. Cargo target directories outside worktrees
#   3. ~/Library/Caches + ~/Library/Application Support/Claude/Cache (rebuildable caches)
#   4. Orphaned partial downloads in scratch dirs (.part, .ytdl)
#   5. Docker unused volumes and dangling images via `docker prune`
#   6. Nix store garbage — runs `nix store gc` in --delete mode
#   7. APFS Time Machine local snapshots — INFORMATIONAL ONLY (script does not use sudo).
#      The script prints copy-pasteable `sudo tmutil deletelocalsnapshots ...` commands.
#
# A worktree is KEPT if any of the following holds:
#   (a) its path is the work_dir of a task with status 'running' in cc.db
#   (b) its basename ends in the id prefix of a running task (fallback matching)
#   (c) any live tmux pane has its cwd inside the worktree
#   (d) it has uncommitted changes — ignoring runner-generated files
#       (.mcp.json, .envrc, .claude/, .direnv/)
#
# Fail-closed: if sqlite3 is missing or the cc.db query fails, the whole
# worktree section is SKIPPED rather than treating every worktree as stale.
#
# After worktree removal, runs `git worktree prune`.
#
# Usage:
#   ./scripts/disk-cleanup.sh          # dry-run (default)
#   ./scripts/disk-cleanup.sh --delete  # actually delete
#
set -euo pipefail

if ((BASH_VERSINFO[0] < 4)); then
  echo "ERROR: bash >= 4 required (associative arrays); found $BASH_VERSION" >&2
  exit 1
fi

DB="${CLAT_DB:-$HOME/projects/bodymindarts/command-center/data/cc.db}"
PROJECTS_DIR="${PROJECTS_DIR:-$HOME/projects}"
MODE="${1:-dry-run}"

case "$MODE" in
  dry-run | --delete) ;;
  *)
    echo "Usage: $0 [--delete]   (default: dry-run)" >&2
    exit 1
    ;;
esac

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

# Format a KB count as human-readable. Lets callers run `du` once instead of
# once for -sk and again for -sh — the du pass dominates this script's runtime.
human_kb() {
  awk -v kb="${1:-0}" 'BEGIN {
    if (kb >= 1048576) printf "%.1fG", kb/1048576
    else if (kb >= 1024) printf "%.0fM", kb/1024
    else printf "%dK", kb
  }'
}

# Size of a path in KB, 0 if unreadable/missing.
size_of_kb() {
  local kb
  kb=$(du -sk "$1" 2>/dev/null | cut -f1) || kb=""
  echo "${kb:-0}"
}

# Just-in-time liveness re-check for a worktree, run immediately before deleting.
# The startup snapshot of running tasks goes stale during a long run (the du pass
# takes minutes) and new tasks spawn frequently, so re-ask before destroying
# anything. Returns 0 (live -> keep) on any error: fail closed.
wt_is_live_now() {
  local dir="${1%/}" prefix="$2" hit esc_dir esc_prefix
  esc_dir=${dir//\'/\'\'}
  esc_prefix=${prefix//\'/\'\'}

  hit=$(sqlite3 -readonly "$DB" \
    "SELECT 1 FROM tasks
       WHERE status = 'running' AND deleted = 0
         AND (rtrim(COALESCE(work_dir, ''), '/') = '$esc_dir'
              OR ('$esc_prefix' <> '' AND substr(id, 1, 8) = '$esc_prefix'))
       LIMIT 1;" 2>/dev/null) || return 0
  [[ -n "$hit" ]] && return 0

  if command -v tmux >/dev/null 2>&1; then
    local cwd
    while IFS= read -r cwd; do
      if [[ -n "$cwd" && ( "$cwd" == "$dir" || "$cwd" == "$dir"/* ) ]]; then
        return 0
      fi
    done < <(tmux list-panes -a -F '#{pane_current_path}' 2>/dev/null || true)
  fi

  return 1
}

# Is a repo checkout actively in use? Used to protect its target/ dir from being
# deleted out from under a running build. Deliberately stricter than
# wt_is_live_now: a pane inside repo/.claude/worktrees/* does NOT count, since a
# worktree has its own target/ and never builds into the parent repo's.
# Returns 0 (busy -> keep) on any error: fail closed.
repo_is_busy() {
  local repo="${1%/}" hit esc_repo
  esc_repo=${repo//\'/\'\'}

  hit=$(sqlite3 -readonly "$DB" \
    "SELECT 1 FROM tasks
       WHERE status = 'running' AND deleted = 0
         AND rtrim(COALESCE(work_dir, ''), '/') = '$esc_repo'
       LIMIT 1;" 2>/dev/null) || return 0
  [[ -n "$hit" ]] && return 0

  if command -v tmux >/dev/null 2>&1; then
    local cwd
    while IFS= read -r cwd; do
      [[ -n "$cwd" ]] || continue
      [[ "$cwd" == "$repo"/.claude/worktrees/* ]] && continue
      if [[ "$cwd" == "$repo" || "$cwd" == "$repo"/* ]]; then
        return 0
      fi
    done < <(tmux list-panes -a -F '#{pane_current_path}' 2>/dev/null || true)
  fi

  return 1
}

echo -e "${BOLD}=== Disk Cleanup ===${NC}"
echo -e "Mode: ${BOLD}$MODE${NC}"
df -h / | tail -1 | awk '{printf "Disk: %s used / %s total (%s free)\n", $3, $2, $4}'
echo ""

########################################################################
# 1. STALE WORKTREES
########################################################################
echo -e "${BOLD}--- [1/7] Stale Claude Worktrees ---${NC}"

db_ok=0
running_rows=""
if ! command -v sqlite3 >/dev/null 2>&1; then
  echo -e "${RED}WARNING: sqlite3 not found — skipping worktree cleanup (fail-closed)${NC}" >&2
elif [[ ! -f "$DB" ]]; then
  echo -e "${RED}WARNING: Database not found at $DB — skipping worktree cleanup${NC}" >&2
elif ! running_rows=$(sqlite3 -readonly "$DB" \
  "SELECT substr(id, 1, 8) || '|' || COALESCE(work_dir, '') FROM tasks WHERE status = 'running' AND deleted = 0;"); then
  echo -e "${RED}WARNING: query against $DB failed — skipping worktree cleanup (fail-closed)${NC}" >&2
else
  db_ok=1
fi

if [[ "$db_ok" == "1" ]]; then
  # Running tasks: exact work_dirs (primary guard) + id prefixes (fallback,
  # id prefixes are not guaranteed unique — collisions err toward keeping)
  declare -A RUNNING_TASKS
  declare -A RUNNING_WORK_DIRS
  while IFS='|' read -r prefix work_dir; do
    [[ -n "$prefix" ]] && RUNNING_TASKS["$prefix"]=1
    [[ -n "$work_dir" ]] && RUNNING_WORK_DIRS["${work_dir%/}"]=1
  done <<<"$running_rows"

  # Collect cwds of all live tmux panes (across all sessions/servers reachable)
  declare -A LIVE_CWDS
  if command -v tmux >/dev/null 2>&1; then
    while IFS= read -r pane_cwd; do
      [[ -n "$pane_cwd" ]] && LIVE_CWDS["$pane_cwd"]=1
    done < <(tmux list-panes -a -F '#{pane_current_path}' 2>/dev/null || true)
  fi

  wt_delete_count=0
  wt_keep_count=0
  wt_dirty_count=0
  wt_bytes=0
  declare -A AFFECTED_REPOS

  while IFS= read -r worktree_dir; do
    base=$(basename "$worktree_dir")
    id_prefix=$(echo "$base" | grep -oE '019[a-f0-9]{5}$' || echo "")

    # Keep if this exact path is the work_dir of a running task
    if [[ -n "${RUNNING_WORK_DIRS[$worktree_dir]:-}" ]]; then
      wt_keep_count=$((wt_keep_count + 1))
      continue
    fi

    # Keep if task is running in DB (id-prefix fallback)
    if [[ -n "$id_prefix" ]] && [[ -n "${RUNNING_TASKS[$id_prefix]:-}" ]]; then
      wt_keep_count=$((wt_keep_count + 1))
      continue
    fi

    # Keep if any live tmux pane has a cwd inside this worktree
    keep_for_tmux=0
    for cwd in "${!LIVE_CWDS[@]}"; do
      if [[ "$cwd" == "$worktree_dir" || "$cwd" == "$worktree_dir"/* ]]; then
        keep_for_tmux=1
        break
      fi
    done
    if [[ "$keep_for_tmux" == "1" ]]; then
      echo -e "  ${GREEN}KEEP${NC}  (live tmux pane)\t$base"
      wt_keep_count=$((wt_keep_count + 1))
      continue
    fi

    # Skip if dir was already removed by deletion of an outer worktree
    [[ -d "$worktree_dir" ]] || continue

    # Keep if there are uncommitted changes (ignoring runner-generated files)
    dirty=$(git -C "$worktree_dir" status --porcelain 2>/dev/null |
      grep -vE '^.. (\.mcp\.json$|\.envrc$|\.claude/|\.direnv/)' || true)
    if [[ -n "$dirty" ]]; then
      echo -e "  ${YELLOW}KEEP${NC}  (uncommitted changes)\t$base"
      wt_dirty_count=$((wt_dirty_count + 1))
      continue
    fi

    size_kb=$(size_of_kb "$worktree_dir")
    size_human=$(human_kb "$size_kb")

    # Re-check liveness immediately before destroying anything
    if [[ "$MODE" == "--delete" ]] && wt_is_live_now "$worktree_dir" "$id_prefix"; then
      echo -e "  ${GREEN}KEEP${NC}  (task started during run)\t$base"
      wt_keep_count=$((wt_keep_count + 1))
      continue
    fi

    wt_bytes=$((wt_bytes + size_kb))
    wt_delete_count=$((wt_delete_count + 1))

    parent_repo=$(echo "$worktree_dir" | sed 's|/\.claude/worktrees/.*||')
    AFFECTED_REPOS["$parent_repo"]=1

    if [[ "$MODE" == "--delete" ]]; then
      rm -rf "$worktree_dir"
      echo -e "  ${RED}DEL${NC}  ${size_human}\t$base"
    else
      echo -e "  ${YELLOW}STALE${NC}  ${size_human}\t$base"
    fi
  done < <(find "$PROJECTS_DIR" -maxdepth 8 -type d -path '*/.claude/worktrees/*' 2>/dev/null \
            | awk -F/ '$(NF-1) == "worktrees" && $(NF-2) == ".claude" { print }' \
            | sort)

  # Prune git worktree metadata
  if [[ "$MODE" == "--delete" ]]; then
    for repo in "${!AFFECTED_REPOS[@]}"; do
      if [[ -d "$repo/.git" ]]; then
        git -C "$repo" worktree prune 2>/dev/null || true
      fi
    done
  fi

  wt_human=$(human_kb "$wt_bytes")
  echo -e "  Keeping: ${GREEN}$wt_keep_count${NC} (running tasks / live panes)"
  echo -e "  Dirty:   ${YELLOW}$wt_dirty_count${NC} (uncommitted changes — kept, inspect manually)"
  echo -e "  Stale:   ${RED}$wt_delete_count${NC} ($wt_human)"
  echo ""
fi

########################################################################
# 2. CARGO TARGET DIRECTORIES
########################################################################
echo -e "${BOLD}--- [2/7] Cargo Target Directories ---${NC}"

cargo_bytes=0
cargo_count=0

while IFS= read -r target_dir; do
  # Only remove real cargo build dirs: cargo drops CACHEDIR.TAG inside target/,
  # and the crate root next to it has a Cargo.toml. Anything else is skipped.
  if [[ ! -f "$target_dir/CACHEDIR.TAG" && ! -f "$(dirname "$target_dir")/Cargo.toml" ]]; then
    echo -e "  ${GREEN}SKIP${NC}  (no Cargo.toml/CACHEDIR.TAG)\t$target_dir"
    continue
  fi

  # Never yank target/ out from under an agent working in that checkout
  if repo_is_busy "$(dirname "$target_dir")"; then
    echo -e "  ${GREEN}KEEP${NC}  (repo in use)\t$target_dir"
    continue
  fi

  size_kb=$(size_of_kb "$target_dir")
  size_human=$(human_kb "$size_kb")
  cargo_bytes=$((cargo_bytes + size_kb))
  cargo_count=$((cargo_count + 1))

  if [[ "$MODE" == "--delete" ]]; then
    rm -rf "$target_dir"
    echo -e "  ${RED}DEL${NC}  ${size_human}\t$target_dir"
  else
    echo -e "  ${YELLOW}FOUND${NC}  ${size_human}\t$target_dir"
  fi
done < <(find "$PROJECTS_DIR" -name target -type d -maxdepth 3 2>/dev/null | sort)

cargo_human=$(human_kb "$cargo_bytes")
echo -e "  Total: ${RED}$cargo_count${NC} dirs ($cargo_human)"
echo ""

########################################################################
# 3. LIBRARY CACHES
########################################################################
echo -e "${BOLD}--- [3/7] ~/Library/Caches ---${NC}"

CACHE_DIRS=(
  "$HOME/Library/Caches/Cypress"
  "$HOME/Library/Caches/ms-playwright"
  "$HOME/Library/Caches/pip"
  "$HOME/Library/Caches/Yarn"
  "$HOME/Library/Caches/pnpm"
  "$HOME/Library/Caches/drata-agent-updater"
  "$HOME/Library/Caches/deno"
  "$HOME/Library/Caches/node-gyp"
  "$HOME/Library/Caches/com.anthropic.claudefordesktop.ShipIt"
  "$HOME/Library/Application Support/Claude/Cache"
  "$HOME/Library/Application Support/Claude/Code Cache"
  "$HOME/Library/Application Support/Claude/GPUCache"
)

cache_bytes=0
cache_count=0

for cache_dir in "${CACHE_DIRS[@]}"; do
  if [[ -d "$cache_dir" ]]; then
    size_kb=$(size_of_kb "$cache_dir")
    size_human=$(human_kb "$size_kb")
    cache_bytes=$((cache_bytes + size_kb))
    cache_count=$((cache_count + 1))

    if [[ "$MODE" == "--delete" ]]; then
      rm -rf "$cache_dir"
      echo -e "  ${RED}DEL${NC}  ${size_human}\t$(basename "$cache_dir")"
    else
      echo -e "  ${YELLOW}FOUND${NC}  ${size_human}\t$(basename "$cache_dir")"
    fi
  fi
done

cache_human=$(human_kb "$cache_bytes")
echo -e "  Total: ${RED}$cache_count${NC} dirs ($cache_human)"
echo ""

########################################################################
# 4. ORPHANED DOWNLOAD PARTS (in scratch dirs)
########################################################################
echo -e "${BOLD}--- [4/7] Orphaned Download Parts ---${NC}"

SCRATCH_DIRS=(
  "$HOME/projects/bodymindarts/command-center/data/scratch"
)

orphan_bytes=0
orphan_count=0

for scratch in "${SCRATCH_DIRS[@]}"; do
  [[ -d "$scratch" ]] || continue
  while IFS= read -r f; do
    size_kb=$(size_of_kb "$f")
    size_human=$(human_kb "$size_kb")
    orphan_bytes=$((orphan_bytes + size_kb))
    orphan_count=$((orphan_count + 1))
    if [[ "$MODE" == "--delete" ]]; then
      rm -f "$f"
      echo -e "  ${RED}DEL${NC}  ${size_human}\t$(basename "$f")"
    else
      echo -e "  ${YELLOW}FOUND${NC}  ${size_human}\t$(basename "$f")"
    fi
  done < <(find "$scratch" \( -name "*.part" -o -name "*.ytdl" -o -name "*.part-Frag*.part" \) -type f 2>/dev/null)
done

orphan_human=$(human_kb "$orphan_bytes")
echo -e "  Total: ${RED}$orphan_count${NC} files ($orphan_human)"
echo ""

########################################################################
# 5. DOCKER — unused volumes and dangling images
########################################################################
echo -e "${BOLD}--- [5/7] Docker Prune ---${NC}"

if ! command -v docker >/dev/null 2>&1; then
  echo -e "  ${YELLOW}docker not installed — skipping${NC}"
elif ! docker info >/dev/null 2>&1; then
  echo -e "  ${YELLOW}docker daemon not running — skipping${NC}"
else
  if [[ "$MODE" == "--delete" ]]; then
    echo -e "  ${RED}Pruning unused volumes...${NC}"
    docker volume prune -f 2>&1 | sed 's/^/    /'
    echo -e "  ${RED}Pruning dangling images...${NC}"
    docker image prune -f 2>&1 | sed 's/^/    /'
    echo -e "  ${RED}Pruning stopped containers...${NC}"
    docker container prune -f 2>&1 | sed 's/^/    /'
    echo -e "  ${RED}Pruning build cache...${NC}"
    docker builder prune -f 2>&1 | sed 's/^/    /'
  else
    echo -e "  Current docker disk usage:"
    docker system df 2>/dev/null | sed 's/^/    /'
  fi
fi
echo ""

########################################################################
# 6. NIX STORE GARBAGE COLLECTION
########################################################################
echo -e "${BOLD}--- [6/7] Nix Store GC ---${NC}"

if ! command -v nix >/dev/null 2>&1; then
  echo -e "  ${YELLOW}nix not installed — skipping${NC}"
else
  if [[ "$MODE" == "--delete" ]]; then
    echo -e "  ${RED}Running nix store gc (may take a while)...${NC}"
    nix store gc 2>&1 | sed 's/^/    /' || true
  else
    echo -e "  ${YELLOW}Would run:${NC} nix store gc"
    echo -e "  (skipped in dry-run; run with --delete to execute)"
  fi
fi
echo ""

########################################################################
# 7. APFS TIME MACHINE LOCAL SNAPSHOTS — INFORMATIONAL ONLY
########################################################################
# Snapshots pin freed bytes — until they're deleted, `df` keeps showing
# the disk as full. This script does not call sudo; it prints the
# commands so you can paste them into a separate shell.
########################################################################
echo -e "${BOLD}--- [7/7] APFS Time Machine Local Snapshots (informational) ---${NC}"

snap_count=0
snap_list=$(tmutil listlocalsnapshots / 2>/dev/null | sed -n 's/^com.apple.TimeMachine\.\(.*\)\.local$/\1/p' || true)

if [[ -z "$snap_list" ]]; then
  echo -e "  ${GREEN}No local snapshots found${NC}"
else
  echo -e "  ${YELLOW}Paste these into a shell to free the pinned space:${NC}"
  while IFS= read -r snap; do
    [[ -z "$snap" ]] && continue
    snap_count=$((snap_count + 1))
    echo "    sudo tmutil deletelocalsnapshots $snap"
  done <<<"$snap_list"
fi
echo -e "  Total: ${RED}$snap_count${NC} snapshots"
echo ""

########################################################################
# SUMMARY
########################################################################
total_kb=$((${wt_bytes:-0} + cargo_bytes + cache_bytes + orphan_bytes))
total_human=$(human_kb "$total_kb")

echo -e "${BOLD}=== Summary ===${NC}"
echo -e "  Space to reclaim: ${RED}${total_human}${NC}"
echo ""

if [[ "$MODE" == "--delete" ]]; then
  echo -e "${GREEN}Done!${NC} Freed ~${total_human}."
  echo ""
  df -h / | tail -1 | awk '{printf "Disk now: %s used / %s total (%s free)\n", $3, $2, $4}'
  echo ""
  if [[ "$snap_count" -gt 0 ]]; then
    echo -e "${YELLOW}Note:${NC} $snap_count APFS local snapshots are pinning freed space."
    echo "      Paste the sudo commands from section [7/7] above to release it."
    echo ""
  fi
else
  echo -e "This was a ${YELLOW}DRY RUN${NC}. To actually delete, run:"
  echo "  $0 --delete"
  echo ""
  echo "After deletion, also:"
  echo "  - Paste the sudo snapshot-delete commands from section [7/7]"
fi
