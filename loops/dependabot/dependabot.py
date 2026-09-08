# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Select one Dependabot repair batch; supervise Astra in its own RimZ worktree."""

import argparse
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


def command(*args, root=ROOT):
    result = subprocess.run(
        args, cwd=root, text=True, capture_output=True, timeout=120, check=False
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


def select(repo, prs, branches, checks):
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


def query_plan():
    repo = github("repo", "view", "--json", "nameWithOwner,defaultBranchRef")
    slug = repo["nameWithOwner"]
    prs = pages(f"repos/{slug}/pulls?state=all&per_page=100")
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
    return select(repo, prs, branches, checks)


def control_worktree():
    # NUL-delimited porcelain preserves spaces, quotes, and newlines in paths.
    listing = command("git", "worktree", "list", "--porcelain", "-z")
    matches = []
    for record in listing.split("\0\0"):
        fields = dict(field.partition(" ")[::2] for field in record.split("\0") if field)
        if fields.get("branch") == "refs/heads/dependabot-loop" and "prunable" not in fields:
            matches.append(Path(fields["worktree"]))
    if len(matches) != 1:
        raise RuntimeError("Create the dedicated control worktree with rimz worktree new dependabot-loop and put the loop code there")
    script = matches[0] / "loops/dependabot/dependabot.py"
    if not script.is_file():
        raise RuntimeError(f"Control worktree is missing {script}; bring the loop code into that worktree")
    return matches[0]


def dispatch():
    control = control_worktree()
    os.chdir(control)
    # Replace the bootstrap, preserving the scheduler's timeout/cancellation tree.
    os.execvp("uv", ["uv", "run", "--no-project", "--script",
                     "loops/dependabot/dependabot.py", "run"])


def require_control_worktree():
    git_dir = command("git", "rev-parse", "--path-format=absolute", "--git-dir").strip()
    common = command("git", "rev-parse", "--path-format=absolute", "--git-common-dir").strip()
    branch = command("git", "branch", "--show-current").strip()
    if Path(git_dir).resolve() == Path(common).resolve() or branch != "dependabot-loop":
        raise RuntimeError("Run the coordinator from the dedicated dependabot-loop linked worktree")
    return Path(common)


def occupied_repair_lane(report):
    if report["schema"] != 1:
        raise RuntimeError("Unsupported rimz agents JSON schema; update the coordinator")
    return [agent["handle"] for agent in report["agents"]
            if (agent["placement"]["branch"] or "").startswith(PREFIX) and agent["placement"]["pane"]]


def launch(plan):
    prompt = (TASK_DIR / "prompt.md").read_text()
    prompt += "\n\nDependency repair plan (JSON):\n" + json.dumps(plan)
    # No shell: titles, prompts, paths, and PR metadata remain argv data.
    subprocess.run(["rimz", "agents", "astra", prompt, "-w", plan["branch"],
                    "-p", "--timeout", "60m"], cwd=ROOT, check=True)


def verify_result(plan, rows):
    if len(rows) != 1:
        raise RuntimeError("Worker must leave exactly one replacement PR; inspect its report before retrying")
    pr = rows[0]
    if marker(branch_sources(plan["branch"])) not in (pr.get("body") or "").splitlines():
        raise RuntimeError(f"Replacement #{pr['number']} lacks its batch marker")
    if pr["state"] == "MERGED":
        return "merged"
    if pr["state"] != "OPEN":
        raise RuntimeError(f"Replacement #{pr['number']} was closed without merging")
    if heads_marker(plan["sources"]) not in (pr.get("body") or "").splitlines():
        raise RuntimeError(f"Replacement #{pr['number']} lacks the verified source revisions")
    state = current_checks({"number": pr["number"], "head": {"sha": pr["headRefOid"]}}, pr)
    if state == "failed":
        raise RuntimeError(f"Replacement #{pr['number']} still has failed CI; the next fire can retry")
    return state


def run():
    common = require_control_worktree()
    with (common / "rimz-dependabot-repair.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(json.dumps(dict(action="idle", reason="another coordinator holds the repository lock")))
            return
        # A supervisor can die while its pane survives. Do not spawn a second worker there.
        occupied = occupied_repair_lane(json.loads(command("rimz", "agents", "list", "--all", "--json")))
        if occupied:
            print(json.dumps(dict(action="idle", reason="repair lane still occupied", agents=occupied)))
            return
        plan = query_plan()  # Re-read after taking the shared lock, never consume a stale plan file.
        print(json.dumps(plan), flush=True)
        if plan["action"] == "repair":
            launch(plan)
            rows = github("pr", "list", "--repo", plan["repo"], "--head", plan["branch"],
                          "--state", "all", "--json", "number,state,body,statusCheckRollup,headRefOid")
            print(json.dumps(dict(replacement_pr=rows[0]["number"] if rows else None,
                                  outcome=verify_result(plan, rows))), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, suggest_on_error=True, color=False)
    parser.add_argument("action", choices=("plan", "run", "dispatch"))
    args = parser.parse_args()
    try:
        if args.action == "plan":
            print(json.dumps(query_plan()))
        elif args.action == "dispatch":
            dispatch()
        else:
            run()
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"Dependabot coordinator failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
