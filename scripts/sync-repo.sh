#!/usr/bin/env bash
set -euo pipefail

branch="${RIMZ_SYNC_REPO_BRANCH:-${RIMZ_MAIN_GIT_BRANCH:-main}}"
remote="${RIMZ_SYNC_REPO_REMOTE:-${RIMZ_MAIN_GIT_REMOTE:-origin}}"
codex_spec="${RIMZ_SYNC_REPO_CODEX_SPEC:-${RIMZ_MAIN_GIT_CODEX_SPEC:-codex}}"
codex_timeout="${RIMZ_SYNC_REPO_CODEX_TIMEOUT:-${RIMZ_MAIN_GIT_CODEX_TIMEOUT:-45m}}"
rimz_bin="${RIMZ_BIN:-rimz}"

usage() {
  printf 'usage: %s [--branch NAME] [--remote NAME] [--codex SPEC] [--timeout DURATION]\n' "$0"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --branch)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      branch="$2"
      shift 2
      ;;
    --remote)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      remote="$2"
      shift 2
      ;;
    --codex)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      codex_spec="$2"
      shift 2
      ;;
    --timeout)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      codex_timeout="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

log() {
  printf 'rimz-sync-repo: %s\n' "$*" >&2
}

die() {
  log "$*"
  exit 1
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "run inside a git repository"
cd "$repo_root"

git_common_dir="$(git rev-parse --git-common-dir)"
case "$git_common_dir" in
  /*) ;;
  *) git_common_dir="$repo_root/$git_common_dir" ;;
esac

lock_dir="$git_common_dir/rimz-sync-repo.lock"
if ! mkdir "$lock_dir" 2>/dev/null; then
  log "another sync-repo run is active; skipping"
  exit 0
fi
trap 'rmdir "$lock_dir" 2>/dev/null || true' EXIT

rebase_in_progress() {
  [ -d "$git_common_dir/rebase-merge" ] || [ -d "$git_common_dir/rebase-apply" ]
}

rebase_branch() {
  for state_dir in "$git_common_dir/rebase-merge" "$git_common_dir/rebase-apply"; do
    [ -f "$state_dir/head-name" ] || continue
    sed 's#^refs/heads/##' "$state_dir/head-name"
    return
  done
}

require_clean_tree() {
  git update-index -q --refresh || true
  git diff --quiet || die "working tree has unstaged changes; skipping"
  git diff --cached --quiet || die "index has staged changes; skipping"
  [ -z "$(git ls-files --others --exclude-standard)" ] || die "working tree has untracked files; skipping"
}

switch_to_branch() {
  current="$(git rev-parse --abbrev-ref HEAD)"
  if [ "$current" = "$branch" ]; then
    return
  fi
  require_clean_tree
  log "switching from $current to $branch"
  git switch "$branch"
}

spawn_codex_for_rebase() {
  command -v "$rimz_bin" >/dev/null 2>&1 || die "rebase conflicts need Codex, but $rimz_bin is not on PATH"

  status_short="$(git status --short --branch)"
  prompt="A scheduled repository sync run hit rebase conflicts.

Repository: $repo_root
Branch: $branch
Remote: $remote/$branch

Current status:
$status_short

Resolve the rebase conflicts in this repository. Preserve both upstream and local intent, follow AGENTS.md, run focused validation when practical, stage the resolved files, and finish the rebase with \`GIT_EDITOR=true git rebase --continue\`. Stop when \`git status --short --branch\` is clean on $branch. Do not push; sync-repo.sh will push after you finish."

  log "spawning $codex_spec to resolve rebase conflicts"
  codex_args=("$rimz_bin" agents "$codex_spec" "$prompt" -p --timeout "$codex_timeout")
  case "${RIMZ_SYNC_REPO_CODEX_YOLO:-${RIMZ_MAIN_GIT_CODEX_YOLO:-}}" in
    1|true|yes)
      codex_args+=(--yolo)
      ;;
  esac
  "${codex_args[@]}"
}

finish_conflicted_rebase() {
  spawn_codex_for_rebase
  if rebase_in_progress; then
    die "Codex returned before the rebase completed"
  fi
  current="$(git rev-parse --abbrev-ref HEAD)"
  [ "$current" = "$branch" ] || die "Codex completed the rebase on $current, expected $branch"
  require_clean_tree
}

git remote get-url "$remote" >/dev/null || die "remote $remote is not configured"

if rebase_in_progress; then
  active_rebase_branch="$(rebase_branch)"
  [ -z "$active_rebase_branch" ] || [ "$active_rebase_branch" = "$branch" ] || die "rebase in progress for $active_rebase_branch, expected $branch"
  finish_conflicted_rebase
  log "pushing completed rebase"
  git push "$remote" "$branch"
  exit 0
fi

switch_to_branch
require_clean_tree

remote_ref="refs/remotes/$remote/$branch"
local_ref="refs/heads/$branch"

log "fetching $remote"
git fetch --prune "$remote"

if ! git show-ref --verify --quiet "$remote_ref"; then
  log "$remote/$branch does not exist; pushing $branch and setting upstream"
  git push -u "$remote" "$branch"
  exit 0
fi

local_oid="$(git rev-parse "$local_ref")"
remote_oid="$(git rev-parse "$remote_ref")"

if git merge-base --is-ancestor "$local_oid" "$remote_oid"; then
  log "pulling $remote/$branch with fast-forward"
  git pull --ff-only "$remote" "$branch"
  log "pushing $branch"
  git push "$remote" "$branch"
elif git merge-base --is-ancestor "$remote_oid" "$local_oid"; then
  log "$branch is ahead of $remote/$branch; pull verifies fast-forward state"
  git pull --ff-only "$remote" "$branch"
  log "pushing $branch"
  git push "$remote" "$branch"
else
  log "$branch diverged from $remote/$branch; rebasing local commits"
  if git rebase "$remote_ref"; then
    log "pushing rebased $branch"
    git push "$remote" "$branch"
  else
    finish_conflicted_rebase
    log "pushing Codex-resolved rebase"
    git push "$remote" "$branch"
  fi
fi

log "done"
