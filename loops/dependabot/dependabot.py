# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Select one Dependabot repair batch after a quiet period; launch spot in a fresh RimZ worktree."""

import argparse
from datetime import datetime, timedelta, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys

TASK_DIR = Path(__file__).resolve().parent
ROOT = TASK_DIR.parents[1]
PREFIX = "deps/repair-"
MARKER = "rimz-dependabot-repair:v1 sources="
SEAT_SKILLS = {"coder": ("commit", "fix-ci"), "reviewer": ("pr",)}


def command(*args, root=None):
    result = subprocess.run(
        args, cwd=root or ROOT, text=True, capture_output=True, timeout=120, check=False
    )
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def github(*args):
    return json.loads(command("gh", *args))


def pages(endpoint):
    return [item for page in github("api", "--paginate", "--slurp", endpoint) for item in page]


def branch_name(numbers):
    return PREFIX + "-".join(map(str, numbers))


def branch_sources(branch):
    if not re.fullmatch(r"deps/repair-[1-9][0-9]*(?:-[1-9][0-9]*)*", branch):
        raise RuntimeError(f"Malformed repair branch {branch}; inspect it before retrying")
    numbers = [int(n) for n in branch.removeprefix(PREFIX).split("-")]
    if numbers != sorted(set(numbers)):
        raise RuntimeError(f"Unsorted or repeated source numbers in {branch}")
    return numbers


def marker(numbers):
    return "<!-- " + MARKER + ",".join(map(str, numbers)) + " -->"


def heads_marker(sources):
    revisions = ",".join(f"{source['number']}@{source['head_sha']}" for source in sources)
    return f"<!-- rimz-dependabot-heads:v1 sources={revisions} -->"


def replacement_sources(pr):
    body = pr.get("body") or ""
    if not pr["head"]["ref"].startswith(PREFIX) and "rimz-dependabot-repair:" not in body:
        return None
    numbers = branch_sources(pr["head"]["ref"])
    if body.count("rimz-dependabot-repair:") != 1 or marker(numbers) not in body.splitlines():
        raise RuntimeError(f"PR #{pr['number']} needs exactly one standalone marker: {marker(numbers)}")
    return numbers


def current_checks(pr, rollup):
    if rollup["headRefOid"] != pr["head"]["sha"]:
        raise RuntimeError(f"PR #{pr['number']} changed head during selection; retry later")
    checks = rollup["statusCheckRollup"] or []
    if not checks:
        return "missing"
    states = []
    for check in checks:
        if check["__typename"] == "CheckRun":
            if check["status"] != "COMPLETED":
                states.append("pending")
                continue
            verdict = check.get("conclusion")
        elif check["__typename"] == "StatusContext":
            verdict = check["state"]
        else:
            raise RuntimeError(f"Unknown check type: {check['__typename']}")
        if verdict in {"FAILURE", "ERROR", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"}:
            states.append("failed")
        elif verdict in {"SUCCESS", "NEUTRAL", "SKIPPED"}:
            states.append("green")
        else:
            states.append("pending")
    # Wait for a stable completed run instead of racing an in-progress rerun.
    return "pending" if "pending" in states else "failed" if "failed" in states else "green"


def select(repo, prs, branches, checks, now, quiet_period):
    base = repo["defaultBranchRef"]["name"]
    result = dict(action="idle", reason="no uncovered failed Dependabot PRs",
                  repo=repo["nameWithOwner"], default_base=base, branch=None,
                  sources=[], existing_replacement_pr=None)
    by_number = {pr["number"]: pr for pr in prs}

    def source(number):
        pr = by_number.get(number)
        if not pr or pr["user"]["login"] != "dependabot[bot]" or pr["base"]["ref"] != base:
            raise RuntimeError(f"Source #{number} is not a Dependabot PR against {base}")
        return pr

    covered, replacements, active = set(), {}, []
    for pr in prs:
        numbers = replacement_sources(pr)
        if numbers is None:
            continue
        branch = pr["head"]["ref"]
        head_repo = (pr["head"].get("repo") or {}).get("full_name")
        if pr["base"]["ref"] != base or head_repo != result["repo"]:
            raise RuntimeError(f"Replacement #{pr['number']} must belong to this repo and target {base}")
        if branch in replacements or covered.intersection(numbers):
            raise RuntimeError("Duplicate or overlapping replacement identities; inspect PR history")
        for number in numbers:
            source(number)
        replacements[branch] = pr["number"]
        covered.update(numbers)  # Includes closed/merged replacements: never recreate a rejected batch.
        if pr["state"] == "open":
            active.append((branch, numbers, pr["number"]))
    for branch in sorted(branches):
        numbers = branch_sources(branch)
        if branch in replacements:
            continue
        if any(source(n)["state"] != "open" or n in covered for n in numbers):
            raise RuntimeError(f"Interrupted branch {branch} covers closed/already covered sources; inspect it")
        active.append((branch, numbers, None))
    if len(active) > 1:
        raise RuntimeError("Multiple active repair batches; resolve open PRs or orphan repair branches")
    if active:
        branch, numbers, replacement = active[0]
    else:
        numbers = sorted(pr["number"] for pr in prs
                         if pr["state"] == "open" and pr["user"]["login"] == "dependabot[bot]"
                         and pr["base"]["ref"] == base and pr["number"] not in covered
                         and checks.get(pr["number"]) == "failed")
        if not numbers:
            return result
        if quiet_period > timedelta(0):
            newest = max((pr for pr in prs if pr["state"] == "open" and pr["user"]["login"] == "dependabot[bot]"),
                         key=lambda pr: datetime.fromisoformat(pr["created_at"]))
            created = datetime.fromisoformat(newest["created_at"])
            ready = created + quiet_period
            if now < ready:
                result["reason"] = (f"Dependabot active: newest open PR #{newest['number']} created {created.isoformat()}; "
                                    f"a new batch waits until {ready.isoformat()}")
                return result
        branch, replacement = branch_name(numbers), None
    result.update(branch=branch, existing_replacement_pr=replacement,
                  sources=[dict(number=n, head_sha=source(n)["head"]["sha"], title=source(n)["title"])
                           for n in numbers])
    if replacement is not None:
        state = checks[replacement]
        revisions_current = heads_marker(result["sources"]) in (by_number[replacement].get("body") or "").splitlines()
        if not revisions_current:
            result["reason"] = "reconcile changed or unrecorded source revisions on the existing replacement"
        elif state != "failed":
            result["reason"] = f"replacement checks {state}; waiting"
            return result
        else:
            result["reason"] = "resume failed replacement on its existing branch"
    else:
        result["reason"] = "recover interrupted batch" if branch in branches else "combine uncovered failed PRs"
    result["action"] = "repair"
    return result


def prune_empty_batches(base, open_heads):
    """Delete repair branches that carry no work of their own, so select() only sees real batches."""
    trees = branch_worktrees()
    refs = command("git", "for-each-ref", "--format=%(refname) %(objectname)",
                   "refs/heads/deps/repair-*", "refs/remotes/origin/deps/repair-*")
    for ref, sha in (line.split() for line in refs.splitlines()):
        branch = ref.removeprefix("refs/heads/").removeprefix("refs/remotes/origin/")
        # Deleting an open PR's head would close it, and a closed replacement is never recreated.
        if branch in trees or branch in open_heads:
            continue
        if not git_succeeds("merge-base", "--is-ancestor", ref, f"refs/remotes/origin/{base}"):
            continue
        if ref.startswith("refs/remotes/"):
            command("git", "push", f"--force-with-lease=refs/heads/{branch}:{sha}", "origin", f":refs/heads/{branch}")
            where = "origin"
        else:
            command("git", "branch", "-D", branch)
            where = "local"
        print(json.dumps(dict(pruned_batch_branch=True, branch=branch, where=where)), flush=True)


def repo_view():
    return github("repo", "view", "--json", "nameWithOwner,defaultBranchRef")


def pull_requests(repo):
    return pages(f"repos/{repo['nameWithOwner']}/pulls?state=all&per_page=100")


def query_plan(repo, prs, quiet_period=timedelta(hours=6)):
    slug = repo["nameWithOwner"]
    refs = command("git", "for-each-ref", "--format=%(refname)",
                   "refs/heads/deps/repair-*", "refs/remotes/origin/deps/repair-*")
    branches = {ref.removeprefix("refs/heads/").removeprefix("refs/remotes/origin/")
                for ref in refs.splitlines()}
    branches.update(ref["ref"].removeprefix("refs/heads/")
                    for ref in pages(f"repos/{slug}/git/matching-refs/heads/deps/repair-"))
    checks = {}
    for pr in prs:
        if pr["state"] == "open" and (pr["user"]["login"] == "dependabot[bot]" or replacement_sources(pr)):
            rollup = github("pr", "view", str(pr["number"]), "--repo", slug,
                            "--json", "statusCheckRollup,headRefOid")
            checks[pr["number"]] = current_checks(pr, rollup)
    return select(repo, prs, branches, checks, datetime.now(timezone.utc), quiet_period)


def rimz(*args):
    return command("rimz", *args)


def git_succeeds(*args):
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, timeout=120, check=False).returncode == 0


def branch_worktrees():
    # NUL-delimited porcelain preserves spaces, quotes, and newlines in paths.
    listing = command("git", "worktree", "list", "--porcelain", "-z")
    trees = {}
    for record in listing.split("\0\0"):
        fields = dict(field.partition(" ")[::2] for field in record.split("\0") if field)
        if fields.get("branch", "").startswith("refs/heads/") and "prunable" not in fields:
            trees[fields["branch"].removeprefix("refs/heads/")] = fields["worktree"]
    return trees


def open_checkout(plan):
    """Cut a fresh checkout from the published branch, never relaunch into a previous cohort's tree."""
    branch = plan["branch"]
    trees = branch_worktrees()
    stale = [dict(branch=other, path=path) for other, path in sorted(trees.items())
             if other.startswith(PREFIX) and other != branch]
    if branch in trees:
        raise RuntimeError(f"checkout {trees[branch]} on {branch} holds unpushed or uncommitted work with no live agent; "
                           f"push it, or discard it with rimz worktree remove {branch} --force")
    published = f"origin/{branch}"
    if not git_succeeds("rev-parse", "--verify", "--quiet", f"refs/remotes/{published}"):
        # Publishing first makes an interrupted first attempt recoverable by name.
        command("git", "push", "origin", f"origin/{plan['default_base']}:refs/heads/{branch}")
        command("git", "fetch", "origin", branch)
    if git_succeeds("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}"):
        if not git_succeeds("merge-base", "--is-ancestor", f"refs/heads/{branch}", published):
            raise RuntimeError(f"Local branch {branch} holds commits absent from {published}; push or delete it")
        command("git", "branch", "-D", branch)
    # A base of origin/<branch> makes RimZ's landed proof mean "every commit is pushed".
    rimz("worktree", "new", branch, "--base", published)
    return dict(attempt_checkout="created", branch=branch, path=branch_worktrees().get(branch),
                stale_attempt_checkouts=stale)


def reclaim_idle_checkouts():
    """Reclaim old cohort trees before planning, deferring if a stopped lane is still closing."""
    for branch in sorted(branch_worktrees()):
        if not branch.startswith(PREFIX):
            continue
        try:
            rimz("worktree", "remove", branch)
            outcome = dict(attempt_checkout="removed", reason="clean and pushed")
        except RuntimeError as error:
            outcome = dict(attempt_checkout="kept", reason=str(error))
        outcome["branch"] = branch
        print(json.dumps(outcome), flush=True)
        if outcome["attempt_checkout"] == "kept" and "is in use by" in outcome["reason"]:
            return outcome


def finished_repair_lanes(trees, open_heads):
    finished = set()
    for branch, path in trees.items():
        if not branch.startswith(PREFIX) or branch in open_heads:
            continue
        try:
            board = (Path(path) / "blackboard.md").read_text()
        except (OSError, UnicodeError):
            continue
        stage = next((line.removeprefix("Stage:").strip() for line in board.splitlines()
                      if line.startswith("Stage:")), "")
        # Match scratch::parse_board_stage: strip only a valid terminal owner, not whitespace in the name.
        if re.sub(r" \(@[^()\s]+\)$", "", stage) == "Done":
            finished.add(branch)
    return finished


def occupied_repair_lane(report, finished):
    if report["schema"] != 1:
        raise RuntimeError("Unsupported rimz agents JSON schema; update the coordinator")
    return [agent["handle"] for agent in report["agents"]
            if (agent["placement"]["branch"] or "").startswith(PREFIX) and agent["placement"]["pane"]
            and agent["placement"]["branch"] not in finished]


def stop_finished_lanes(report, finished):
    for agent in report["agents"]:
        branch, pane = agent["placement"]["branch"], agent["placement"]["pane"]
        if branch not in finished or not pane:
            continue
        rimz("agents", "stop", pane)
        print(json.dumps(dict(stopped=pane, handle=agent["handle"], branch=branch,
                              reason="lane finished: board Done, no open PR")), flush=True)


def require_seat_skills():
    """Refuse before publishing when either spot seat cannot call its required skills."""
    for role, required in SEAT_SKILLS.items():
        report = json.loads(rimz("agents", "explain", f"spot.{role}", "--json"))
        callable_skills = report["skills"]["callable"]
        if callable_skills is None:  # No list in the profile chain: native discovery, all callable.
            continue
        missing = sorted(set(required) - set(callable_skills))
        if missing:
            raise RuntimeError(f"spot.{role} cannot call required skills {', '.join(missing)}; "
                               "add them to the profile's skills list")


def launch(plan, timeout=300):
    prompt = (TASK_DIR / "prompt.md").read_text()
    prompt += "\n\nDependency repair plan (JSON):\n" + json.dumps(plan)
    # No shell: titles, prompts, paths, and PR metadata remain argv data.
    argv = ["rimz", "teams", "launch", "spot", prompt, "-w", plan["branch"], "--bg"]
    # Inside a loop check, RimZ turns a prompted launch into a blocking run and refuses --bg
    # (docs/guide/loops.md). This coordinator owns the lane itself (occupied_repair_lane,
    # stop_finished_lanes, reclaim_idle_checkouts), so it hands the team off: drop the loop
    # marker and keep the workspace pin (RIMZ_WORKSPACE_ID, RIMZ_PROJECT_ROOT).
    env = {key: value for key, value in os.environ.items() if key != "RIMZ_LOOP_TASK"}
    what = f"rimz teams launch spot -w {plan['branch']}"
    try:
        result = subprocess.run(argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, text=True,
                                capture_output=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        # The exception text repeats argv, and the prompt would push everything else out of the run's output tail.
        raise RuntimeError(f"{what} timed out after {timeout}s") from None
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", file=sys.stderr, flush=True)
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"{what} failed (exit {result.returncode}): {detail}")


def run(quiet_period=timedelta(hours=6)):
    common = Path(command("git", "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
    with (common / "rimz-dependabot-repair.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(json.dumps(dict(action="idle", reason="another coordinator holds the repository lock")))
            return
        report = json.loads(rimz("agents", "list", "--all", "--json"))
        # Re-read under the shared lock; reuse this PR snapshot for occupancy, pruning, and planning.
        repo = repo_view()
        prs = pull_requests(repo)
        open_heads = {pr["head"]["ref"] for pr in prs if pr["state"] == "open"}
        finished = finished_repair_lanes(branch_worktrees(), open_heads)
        occupied = occupied_repair_lane(report, finished)
        if occupied:
            print(json.dumps(dict(action="idle", reason="repair lane still occupied", agents=occupied)))
            return
        stop_finished_lanes(report, finished)
        # A tree directory deleted by hand still pins its branch until its metadata is pruned.
        command("git", "worktree", "prune")
        command("git", "fetch", "--prune", "origin")  # A stale tracking ref would fail the lease delete below.
        closing = reclaim_idle_checkouts()
        if closing:
            print(json.dumps(dict(action="idle", reason="repair lane still closing", branch=closing["branch"],
                                  holder=closing["reason"])), flush=True)
            return
        prune_empty_batches(repo["defaultBranchRef"]["name"], open_heads)
        plan = query_plan(repo, prs, quiet_period)
        print(json.dumps(plan), flush=True)
        if plan["action"] != "repair":
            return
        # Before open_checkout publishes the batch branch, so a refused fire leaves nothing behind.
        require_seat_skills()
        checkout = open_checkout(plan)
        print(json.dumps(checkout), flush=True)
        launch(plan)
        print(json.dumps(dict(launched=f"spot#{plan['branch'].replace('/', '-')}", branch=plan["branch"],
                              worktree=checkout["path"])), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, suggest_on_error=True, color=False)
    parser.add_argument("action", choices=("plan", "run"))
    parser.add_argument("--quiet-period", metavar="HOURS", type=float, default=6,
                        help="defer a new batch until every open Dependabot PR is this old; default: 6, 0 disables")
    args = parser.parse_args()
    try:
        if args.quiet_period < 0:
            raise ValueError("--quiet-period must be nonnegative")
        quiet_period = timedelta(hours=args.quiet_period)
        if args.action == "plan":
            repo = repo_view()
            print(json.dumps(query_plan(repo, pull_requests(repo), quiet_period)))
        else:
            run(quiet_period)
    except (RuntimeError, OSError, ValueError, OverflowError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"Dependabot coordinator failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
