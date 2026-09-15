# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Dependabot loop regression tests for duplicate prevention and per-attempt checkouts."""

from contextlib import ExitStack, redirect_stdout
import fcntl
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dependabot as repair

REPO = {"nameWithOwner": "owner/repo", "defaultBranchRef": {"name": "main"}}


def source(number):
    return dict(number=number, state="open", user={"login": "dependabot[bot]"},
                head={"ref": f"dependabot/{number}", "sha": f"sha-{number}",
                      "repo": {"full_name": "owner/repo"}},
                base={"ref": "main"}, title=f"Bump {number}", body=None)


def replacement(number, sources, state="open"):
    pr = source(number)
    heads = [dict(number=n, head_sha=f"sha-{n}") for n in sources]
    pr.update(state=state, user={"login": "maintainer"},
              body=repair.marker(sources) + "\n" + repair.heads_marker(heads))
    pr["head"]["ref"] = repair.branch_name(sources)
    return pr


def select(prs, checks=None, branches=()):
    return repair.select(REPO, prs, set(branches), checks or {})


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, text=True, capture_output=True, check=True).stdout.strip()


class FakeRimz:
    """Records rimz argv and performs the Git effect the real CLI would."""

    def __init__(self, root, roster=(), refusal=None):
        self.root, self.roster, self.refusal, self.calls = root, list(roster), refusal, []

    def __call__(self, *args):
        self.calls.append(args)
        match args:
            case ("agents", "list", *_):
                return json.dumps(dict(schema=1, agents=self.roster))
            case ("worktree", "new", branch, "--base", base):
                git(self.root, "worktree", "add", "-b", branch, str(self.root.parent / branch.replace("/", "-")), base)
                return f"created {branch}"
            case ("worktree", "remove", branch):
                if self.refusal:
                    raise RuntimeError(f"rimz failed: {self.refusal}")
                git(self.root, "worktree", "remove", str(self.root.parent / branch.replace("/", "-")))
                git(self.root, "branch", "-d", branch)
                return ""
        raise AssertionError(f"unexpected rimz call {args}")


class CheckoutFixture(unittest.TestCase):
    """A bare origin and a primary clone with main published; the coordinator runs in the clone."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        base = Path(directory.name)
        self.origin, self.root = base / "origin.git", base / "project"
        git(base, "init", "--quiet", "--bare", "-b", "main", str(self.origin))
        git(base, "clone", "--quiet", str(self.origin), str(self.root))
        for key, value in (("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false")):
            git(self.root, "config", key, value)
        self.commit("initial")
        git(self.root, "push", "--quiet", "origin", "main")
        self.rimz = FakeRimz(self.root)
        for name, value in (("ROOT", self.root), ("rimz", self.rimz)):
            patcher = patch.object(repair, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.plan = select([source(1)], {1: "failed"})

    def commit(self, message, cwd=None):
        git(cwd or self.root, "commit", "--quiet", "--allow-empty", "-m", message)

    def publish_batch(self):
        git(self.root, "push", "--quiet", "origin", "main:refs/heads/deps/repair-1")
        git(self.root, "fetch", "--quiet", "origin")

    def remote_branch(self, branch="deps/repair-1"):
        return git(self.origin, "for-each-ref", "--format=%(objectname)", f"refs/heads/{branch}")

    def tree(self, branch="deps/repair-1"):
        return self.root.parent / branch.replace("/", "-")

    def run_quietly(self, function, *args):
        output = io.StringIO()
        with redirect_stdout(output):
            try:
                function(*args)
            finally:
                self.lines = [json.loads(line) for line in output.getvalue().splitlines()]
        return self.lines

    def test_fresh_batch_is_published_before_its_checkout_is_cut_from_origin(self):
        self.assertEqual(self.remote_branch(), "")
        report = repair.open_checkout(self.plan)
        self.assertEqual(report["attempt_checkout"], "created")
        self.assertEqual(self.remote_branch(), git(self.origin, "rev-parse", "main"))
        self.assertEqual(self.rimz.calls, [("worktree", "new", "deps/repair-1", "--base", "origin/deps/repair-1")])
        self.assertEqual(Path(report["path"]).resolve(), self.tree().resolve())

    def test_stale_local_branch_already_on_origin_is_replaced_by_a_fresh_checkout(self):
        self.publish_batch()
        git(self.root, "branch", "deps/repair-1", "origin/deps/repair-1")
        self.assertEqual(repair.open_checkout(self.plan)["attempt_checkout"], "created")
        self.assertEqual(git(self.tree(), "rev-parse", "HEAD"), self.remote_branch())

    def test_unpushed_local_branch_stops_before_push_create_or_launch(self):
        self.publish_batch()
        git(self.root, "branch", "deps/repair-1", "origin/deps/repair-1")
        git(self.root, "worktree", "add", "--quiet", str(self.tree()), "deps/repair-1")
        self.commit("unpushed", self.tree())
        git(self.root, "worktree", "remove", str(self.tree()))
        remote = self.remote_branch()
        with self.planned(), \
             patch.object(repair, "launch") as launch, self.assertRaisesRegex(RuntimeError, "push or delete it"):
            self.run_quietly(repair.run)
        launch.assert_not_called()
        self.assertEqual(self.remote_branch(), remote)
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json")])

    def test_hand_deleted_checkout_directory_does_not_fail_the_fire(self):
        self.publish_batch()
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-1", str(self.tree()), "origin/deps/repair-1")
        shutil.rmtree(self.tree())
        with self.planned(), \
             patch.object(repair, "launch", side_effect=subprocess.CalledProcessError(1, "rimz")), \
             self.assertRaises(subprocess.CalledProcessError):
            self.run_quietly(repair.run)
        self.assertIn(dict(pruned_batch_branch=True, branch="deps/repair-1", where="origin"), self.lines)
        self.assertEqual([line["attempt_checkout"] for line in self.lines if "attempt_checkout" in line], ["created", "removed"])

    def planned(self):
        stack = ExitStack()
        stack.enter_context(patch.object(repair, "repo_view", return_value=REPO))
        stack.enter_context(patch.object(repair, "pull_requests", return_value=[]))
        stack.enter_context(patch.object(repair, "query_plan", return_value=self.plan))
        return stack

    def forge(self, prs):
        endpoints = {"repos/owner/repo/pulls?state=all&per_page=100": prs,
                     "repos/owner/repo/git/matching-refs/heads/deps/repair-": []}
        rollups = {str(pr["number"]): dict(headRefOid=pr["head"]["sha"], statusCheckRollup=[]) for pr in prs}
        return (patch.object(repair, "github", side_effect=lambda *args: REPO if args[0] == "repo" else rollups[args[2]]),
                patch.object(repair, "pages", side_effect=endpoints.__getitem__))

    def test_batch_branch_a_worker_left_empty_is_pruned_before_selection(self):
        repair.open_checkout(self.plan)
        self.run_quietly(repair.settle_checkout, "deps/repair-1")
        closed = source(1)
        closed["state"] = "closed"
        github, pages = self.forge([closed])
        with github, pages:
            with self.assertRaisesRegex(RuntimeError, "covers closed"):
                repair.query_plan(REPO, [closed])
            self.assertNotEqual(self.remote_branch(), "")
            self.run_quietly(repair.prune_empty_batches, "main", set())
            self.assertEqual(repair.query_plan(REPO, [closed])["action"], "idle")
        self.assertEqual(self.lines, [dict(pruned_batch_branch=True, branch="deps/repair-1", where="origin")])
        self.assertEqual(git(self.origin, "ls-remote", ".", "refs/heads/deps/repair-1"), "")
        self.assertEqual(git(self.root, "for-each-ref", "refs/remotes/origin/deps/repair-1"), "")

    def test_prune_keeps_branches_with_work_a_tree_or_an_open_pr(self):
        self.publish_batch()
        for branch in ("deps/repair-2", "deps/repair-3", "deps/repair-4"):
            git(self.root, "push", "--quiet", "origin", f"main:refs/heads/{branch}")
        git(self.root, "worktree", "add", "--quiet", "-b", "ahead", str(self.tree("ahead")), "origin/deps/repair-1")
        self.commit("work", self.tree("ahead"))
        git(self.tree("ahead"), "push", "--quiet", "origin", "HEAD:refs/heads/deps/repair-1")
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-2", str(self.tree("deps/repair-2")))
        git(self.root, "branch", "deps/repair-5", "main")
        git(self.root, "fetch", "--quiet", "origin")
        self.run_quietly(repair.prune_empty_batches, "main", {"deps/repair-3"})
        self.assertEqual(sorted((line["branch"], line["where"]) for line in self.lines),
                         [("deps/repair-4", "origin"), ("deps/repair-5", "local")])
        for kept in ("deps/repair-1", "deps/repair-2", "deps/repair-3"):
            self.assertNotEqual(self.remote_branch(kept), "")
        self.assertEqual(git(self.root, "branch", "--list", "deps/repair-5"), "")

    def test_plan_action_never_prunes(self):
        self.publish_batch()
        github, pages = self.forge([source(1)])
        with github, pages, patch.object(repair.sys, "argv", ["dependabot.py", "plan"]), redirect_stdout(io.StringIO()):
            self.assertEqual(repair.main(), 0)
        self.assertNotEqual(self.remote_branch(), "")

    def test_kept_checkout_is_resumed_without_push_or_create(self):
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-1", str(self.tree()))
        report = repair.open_checkout(self.plan)
        self.assertEqual((report["attempt_checkout"], Path(report["path"]).resolve()), ("resumed", self.tree().resolve()))
        self.assertEqual(self.remote_branch(), "")
        self.assertEqual(self.rimz.calls, [])

    def test_other_repair_checkouts_are_reported_stale_and_left_alone(self):
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-2", str(self.tree("deps/repair-2")))
        report = repair.open_checkout(self.plan)
        self.assertEqual([entry["branch"] for entry in report["stale_attempt_checkouts"]], ["deps/repair-2"])
        self.assertTrue(self.tree("deps/repair-2").is_dir())

    def test_settle_trusts_rimz_reclamation_and_liveness_before_one_unforced_remove(self):
        self.run_quietly(repair.settle_checkout, "deps/repair-1")
        self.assertEqual((self.lines[0]["attempt_checkout"], self.rimz.calls), ("removed", []))

        self.publish_batch()
        repair.open_checkout(self.plan)
        self.rimz.calls.clear()
        self.rimz.roster = [dict(handle="@worker", placement=dict(branch="deps/repair-1", pane="tmux:%1"))]
        self.run_quietly(repair.settle_checkout, "deps/repair-1")
        self.assertEqual(self.lines[0], dict(attempt_checkout="kept", reason="worker still live", branch="deps/repair-1"))
        self.assertNotIn("remove", [call[1] for call in self.rimz.calls])

        self.rimz.roster, self.rimz.calls = [], []
        self.run_quietly(repair.settle_checkout, "deps/repair-1")
        self.assertEqual(self.lines[0]["attempt_checkout"], "removed")
        self.assertEqual(self.rimz.calls[-1], ("worktree", "remove", "deps/repair-1"))
        self.assertFalse(self.tree().exists())

    def test_refused_remove_keeps_the_checkout_and_still_verifies_the_pr(self):
        self.rimz.refusal = "worktree `deps-repair-1` has local changes or work not proven landed; use --force to remove it"
        rows = [dict(number=10)]
        with self.planned(), patch.object(repair, "launch"), \
             patch.object(repair, "github", return_value=rows) as github, \
             patch.object(repair, "verify_result", return_value="pending"):
            self.run_quietly(repair.run)
        github.assert_called_once()
        settled = next(line for line in self.lines if line.get("attempt_checkout") in ("kept", "removed"))
        self.assertEqual(settled["attempt_checkout"], "kept")
        self.assertIn("not proven landed", settled["reason"])
        self.assertEqual(self.rimz.calls[-1], ("worktree", "remove", "deps/repair-1"))
        self.assertTrue((self.root / ".git" / "rimz-dependabot-repair.lock").is_file())

    def test_failed_launch_still_settles_and_fails_the_run(self):
        failure = subprocess.CalledProcessError(124, "rimz")
        with self.planned(), \
             patch.object(repair, "launch", side_effect=failure), \
             patch.object(repair, "github") as github, self.assertRaises(subprocess.CalledProcessError):
            self.run_quietly(repair.run)
        github.assert_not_called()
        self.assertEqual([line.get("attempt_checkout") for line in self.lines[1:]], ["created", "removed"])
        self.assertFalse(self.tree().exists())


class RepairTests(unittest.TestCase):
    def test_selects_all_failed_bot_updates_in_stable_order(self):
        human = source(311)
        human["user"]["login"] = "dependabot"
        release = source(312)
        release["base"]["ref"] = "release"
        plan = select([source(310), source(309), human, release],
                      {n: "failed" for n in range(309, 313)})
        self.assertEqual(plan["branch"], "deps/repair-309-310")
        self.assertEqual([p["number"] for p in plan["sources"]], [309, 310])

    def test_pending_green_and_missing_source_checks_do_not_launch(self):
        for state in ("pending", "green", "missing"):
            self.assertEqual(select([source(1)], {1: state})["action"], "idle")

    def test_existing_pr_always_takes_priority_over_new_batch(self):
        prs = [source(1), source(2), replacement(10, [1])]
        for status in ("pending", "green", "missing", "failed"):
            plan = select(prs, {1: "failed", 2: "failed", 10: status})
            self.assertEqual(plan["branch"], "deps/repair-1")
            self.assertEqual(plan["existing_replacement_pr"], 10)
            self.assertEqual(plan["action"], "repair" if status == "failed" else "idle")

    def test_closed_replacement_is_not_recreated(self):
        plan = select([source(1), source(2), replacement(10, [1], "closed")],
                      {1: "failed", 2: "failed"}, ["deps/repair-1"])
        self.assertEqual(plan["branch"], "deps/repair-2")

    def test_green_replacement_is_revisited_when_dependabot_changes_its_head(self):
        changed = source(1)
        changed["head"]["sha"] = "new-head"
        plan = select([changed, replacement(10, [1])], {1: "failed", 10: "green"})
        self.assertEqual(plan["action"], "repair")
        self.assertEqual(plan["existing_replacement_pr"], 10)
        self.assertIn("reconcile", plan["reason"])

    def test_worker_cannot_report_success_without_publishing(self):
        plan = select([source(1)], {1: "failed"})
        with self.assertRaises(RuntimeError):
            repair.verify_result(plan, [])
        result = dict(number=10, state="OPEN", body=replacement(10, [1])["body"],
                      headRefOid="replacement-head", statusCheckRollup=[])
        self.assertEqual(repair.verify_result(plan, [result]), "missing")
        result["statusCheckRollup"] = [dict(__typename="StatusContext", state="FAILURE")]
        with self.assertRaises(RuntimeError):
            repair.verify_result(plan, [result])

    def test_interrupted_branch_recovers_exact_batch(self):
        plan = select([source(1), source(2)], {1: "pending", 2: "failed"}, ["deps/repair-1"])
        self.assertEqual(plan["branch"], "deps/repair-1")
        self.assertEqual(plan["reason"], "recover interrupted batch")

    def test_ambiguous_and_malformed_identities_fail(self):
        for branch in ("deps/repair-", "deps/repair-0", "deps/repair-01", "deps/repair-2-1", "deps/repair-1-1"):
            with self.assertRaises(RuntimeError):
                repair.branch_sources(branch)
        for other in (replacement(11, [1]), replacement(11, [1, 2]), replacement(11, [2])):
            with self.assertRaises(RuntimeError):
                select([source(1), source(2), replacement(10, [1]), other])
        invalid = replacement(10, [1])
        invalid["body"] = repair.marker([2])
        with self.assertRaises(RuntimeError):
            select([source(1), invalid])
        with self.assertRaises(RuntimeError):
            select([source(1), source(2)], branches=["deps/repair-1", "deps/repair-2"])

    def test_current_head_and_complete_checks_are_required(self):
        rollup = dict(headRefOid="old", statusCheckRollup=[])
        with self.assertRaises(RuntimeError):
            repair.current_checks(source(1), rollup)
        rollup["headRefOid"] = "sha-1"
        self.assertEqual(repair.current_checks(source(1), rollup), "missing")
        for conclusion in ("FAILURE", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"):
            rollup["statusCheckRollup"] = [dict(__typename="CheckRun", status="COMPLETED", conclusion=conclusion)]
            self.assertEqual(repair.current_checks(source(1), rollup), "failed")
        rollup["statusCheckRollup"].append(dict(__typename="StatusContext", state="PENDING"))
        self.assertEqual(repair.current_checks(source(1), rollup), "pending")
        rollup["statusCheckRollup"] = [dict(__typename="StatusContext", state="ERROR")]
        self.assertEqual(repair.current_checks(source(1), rollup), "failed")

    def test_pagination_keeps_every_page(self):
        with patch.object(repair, "github", return_value=[[1, 2], [3]]) as gh:
            self.assertEqual(repair.pages("endpoint"), [1, 2, 3])
            gh.assert_called_once_with("api", "--paginate", "--slurp", "endpoint")

    def test_lock_contention_does_not_query_or_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            common = Path(directory)
            with (common / "rimz-dependabot-repair.lock").open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with patch.object(repair, "command", return_value=f"{common}\n"), \
                     patch.object(repair, "query_plan") as query, \
                     patch.object(repair, "launch") as launch, redirect_stdout(io.StringIO()):
                    repair.run()
                    query.assert_not_called()
                    launch.assert_not_called()

    def test_surviving_worker_prevents_duplicate_launch_after_supervisor_death(self):
        report = dict(schema=1, agents=[dict(handle="@worker", placement=dict(branch="deps/repair-1", pane="tmux:%1"))])
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(repair, "command", return_value=f"{directory}\n"), \
             patch.object(repair, "rimz", return_value=json.dumps(report)), \
             patch.object(repair, "query_plan") as query, \
             patch.object(repair, "launch") as launch, redirect_stdout(io.StringIO()):
            repair.run()
            query.assert_not_called()
            launch.assert_not_called()

    def test_launch_passes_prompt_as_data_in_unattended_named_worktree(self):
        plan = select([source(1)], {1: "failed"})
        plan["sources"][0]["title"] = "$(do-not-execute) `neither-this`"
        with patch.object(repair.subprocess, "run") as run:
            repair.launch(plan)
        args, = run.call_args.args
        self.assertEqual(args[:3], ["rimz", "agents", "astra"])
        self.assertIn("$(do-not-execute)", args[3])
        self.assertEqual(args[4:], ["-w", "deps/repair-1", "--yolo", "-p", "--timeout", "60m"])
        self.assertNotIn("shell", run.call_args.kwargs)
        self.assertEqual(run.call_args.kwargs["cwd"], repair.ROOT)


if __name__ == "__main__":
    unittest.main()
