# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Dependabot loop regression tests for duplicate prevention and isolated dispatch."""

from contextlib import redirect_stdout
import fcntl
import io
import json
from pathlib import Path
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


class RepairTests(unittest.TestCase):
    def test_dispatch_finds_control_worktree_without_shell_parsing_paths(self):
        with tempfile.TemporaryDirectory(prefix="loop ' quoted\n") as directory:
            root = Path(directory)
            script = root / "loops/dependabot/dependabot.py"
            script.parent.mkdir(parents=True)
            script.touch()
            listing = ("worktree /primary\0HEAD abc\0branch refs/heads/main\0\0"
                       f"worktree {root}\0HEAD def\0branch refs/heads/dependabot-loop\0\0")
            with patch.object(repair, "command", return_value=listing), \
                 patch.object(repair.os, "chdir") as chdir, \
                 patch.object(repair.os, "execvp") as execute:
                repair.dispatch()
            chdir.assert_called_once_with(root)
            execute.assert_called_once_with("uv", ["uv", "run", "--no-project", "--script",
                                                  "loops/dependabot/dependabot.py", "run"])

    def test_dispatch_refuses_missing_prunable_or_ambiguous_control_worktrees(self):
        record = "worktree /absent\0HEAD abc\0branch refs/heads/dependabot-loop\0"
        for listing in ("", record + "\0", record + "prunable stale\0\0", record + "\0" + record + "\0"):
            with patch.object(repair, "command", return_value=listing), \
                 patch.object(repair.os, "chdir") as chdir, \
                 patch.object(repair.os, "execvp") as execute, self.assertRaises(RuntimeError):
                repair.dispatch()
            chdir.assert_not_called()
            execute.assert_not_called()

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

    def test_primary_checkout_and_wrong_control_branch_are_refused(self):
        for outputs in (("/repo/.git", "/repo/.git", "main"),
                        ("/repo/.git/worktrees/other", "/repo/.git", "other")):
            with patch.object(repair, "command", side_effect=outputs), self.assertRaises(RuntimeError):
                repair.require_control_worktree()

    def test_lock_contention_does_not_query_or_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            common = Path(directory)
            with (common / "rimz-dependabot-repair.lock").open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with patch.object(repair, "require_control_worktree", return_value=common), \
                     patch.object(repair, "query_plan") as query, \
                     patch.object(repair, "launch") as launch, redirect_stdout(io.StringIO()):
                    repair.run()
                    query.assert_not_called()
                    launch.assert_not_called()

    def test_surviving_worker_prevents_duplicate_launch_after_supervisor_death(self):
        report = dict(schema=1, agents=[dict(handle="@worker", placement=dict(branch="deps/repair-1", pane="tmux:%1"))])
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(repair, "require_control_worktree", return_value=Path(directory)), \
             patch.object(repair, "command", return_value=json.dumps(report)), \
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
