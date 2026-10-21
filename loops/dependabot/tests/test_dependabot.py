# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Dependabot loop regression tests for duplicate prevention and per-attempt checkouts."""

from contextlib import ExitStack, redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone
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
NOW = datetime(2026, 10, 13, 12, tzinfo=timezone.utc)
QUIET_PERIOD = timedelta(hours=6)


def source(number, created_at="2026-10-12T00:00:00Z"):
    return dict(number=number, state="open", user={"login": "dependabot[bot]"},
                head={"ref": f"dependabot/{number}", "sha": f"sha-{number}",
                      "repo": {"full_name": "owner/repo"}},
                base={"ref": "main"}, title=f"Bump {number}", body=None, created_at=created_at)


def replacement(number, sources, state="open"):
    pr = source(number)
    heads = [dict(number=n, head_sha=f"sha-{n}") for n in sources]
    pr.update(state=state, user={"login": "maintainer"},
              body=repair.marker(sources) + "\n" + repair.heads_marker(heads))
    pr["head"]["ref"] = repair.branch_name(sources)
    return pr


def select(prs, checks=None, branches=(), quiet_period=QUIET_PERIOD):
    return repair.select(REPO, prs, set(branches), checks or {}, NOW, quiet_period)


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, text=True, capture_output=True, check=True).stdout.strip()


class FakeRimz:
    """Records rimz argv and performs the Git effect the real CLI would."""

    def __init__(self, root, roster=(), refusal=None):
        self.root, self.roster, self.refusal, self.calls = root, list(roster), refusal, []
        self.skills = {"coder": ["commit", "fix-ci"], "reviewer": ["pr"]}
        self.stop_refusal = None

    def __call__(self, *args):
        self.calls.append(args)
        match args:
            case ("agents", "list", "--all", "--json"):
                return json.dumps(dict(schema=1, agents=self.roster))
            case ("agents", "stop", pane):
                if self.stop_refusal:
                    raise RuntimeError(f"rimz failed: {self.stop_refusal}")
                self.roster = [agent for agent in self.roster if agent["placement"]["pane"] != pane]
                return ""
            case ("agents", "explain", seat, "--json") if seat in ("spot.coder", "spot.reviewer"):
                return json.dumps(dict(skills=dict(callable=self.skills[seat.split(".")[1]], applied=True, host=None)))
            case ("worktree", "new", branch, "--base", base):
                git(self.root, "worktree", "add", "-b", branch, str(self.root.parent / branch.replace("/", "-")), base)
                return f"created {branch}"
            case ("worktree", "remove", branch):
                if self.refusal:
                    raise RuntimeError(f"rimz failed: {self.refusal}")
                git(self.root, "worktree", "remove", str(self.root.parent / branch.replace("/", "-")))
                git(self.root, "branch", "-d", branch)
                return ""
            case ("teams", "launch", "spot", _prompt, "-w", _branch, "--bg"):
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
        (self.root / ".git" / "info" / "exclude").write_text("/.rimz/team\n")
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
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json"),
                                           ("agents", "explain", "spot.coder", "--json"),
                                           ("agents", "explain", "spot.reviewer", "--json")])

    def test_hand_deleted_checkout_directory_does_not_fail_the_fire(self):
        self.publish_batch()
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-1", str(self.tree()), "origin/deps/repair-1")
        shutil.rmtree(self.tree())
        with self.planned(), \
             patch.object(repair, "launch", side_effect=RuntimeError("launch failed")), \
             self.assertRaisesRegex(RuntimeError, "launch failed"):
            self.run_quietly(repair.run)
        self.assertIn(dict(pruned_batch_branch=True, branch="deps/repair-1", where="origin"), self.lines)
        self.assertEqual([line["attempt_checkout"] for line in self.lines if "attempt_checkout" in line], ["created"])
        self.assertTrue(self.tree().exists())

    def planned(self, prs=()):
        stack = ExitStack()
        stack.enter_context(patch.object(repair, "repo_view", return_value=REPO))
        stack.enter_context(patch.object(repair, "pull_requests", return_value=list(prs)))
        stack.enter_context(patch.object(repair, "query_plan", return_value=self.plan))
        stack.enter_context(patch.object(repair, "github", side_effect=AssertionError("run must not query GitHub after planning")))
        return stack

    def occupy_checkout(self, board="Stage: Done (@coder)\n"):
        repair.open_checkout(self.plan)
        (self.tree() / ".rimz" / "team").mkdir(parents=True, exist_ok=True)
        if board is not None:
            (self.tree() / ".rimz" / "team" / "blackboard.md").write_text(board)
        self.rimz.roster = [dict(handle="coder", team="spot", role="coder",
                                placement=dict(branch="deps/repair-1", pane="tmux:%1", pr=None))]
        self.rimz.calls.clear()

    def test_surviving_pane_prevents_duplicate_launch_on_unfinished_lanes(self):
        self.occupy_checkout()
        board_path = self.tree() / ".rimz" / "team" / "blackboard.md"
        cases = [("Stage: Review (@reviewer)\n", []),
                 ("Stage: Done (@coder)\n", [replacement(10, [1])]),
                 (None, []), ("# No stage\n", []),
                 ("Stage: Review\nStage: Done\n", []),
                 (" Stage: Done\n", []), ("Stage: Done (@)\n", []),
                 ("Stage: Done (@bad owner)\n", []), ("Stage: Done (@bad(owner))\n", []),
                 ("Stage: Done  (@coder)\n", [])]
        for board, prs in cases:
            with self.subTest(board=board, open_pr=bool(prs)):
                if board is None:
                    board_path.unlink(missing_ok=True)
                else:
                    board_path.write_text(board)
                self.rimz.calls.clear()
                with self.planned(prs), patch.object(repair, "query_plan") as query, \
                     patch.object(repair, "launch") as launch:
                    self.run_quietly(repair.run)
                self.assertEqual(self.lines, [dict(action="idle", reason="repair lane still occupied", agents=["coder"])])
                self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json")])
                query.assert_not_called()
                launch.assert_not_called()
                self.assertTrue(self.tree().exists())
        board_path.unlink()
        board_path.mkdir()
        with self.planned(), patch.object(repair, "launch") as launch:
            self.run_quietly(repair.run)
        self.assertEqual(self.lines[0]["reason"], "repair lane still occupied")
        launch.assert_not_called()

    def test_repair_pane_without_a_checkout_remains_occupied(self):
        self.rimz.roster = [dict(handle="coder", placement=dict(branch="deps/repair-1", pane="tmux:%1"))]
        with self.planned(), patch.object(repair, "launch") as launch:
            self.run_quietly(repair.run)
        self.assertEqual(self.lines, [dict(action="idle", reason="repair lane still occupied", agents=["coder"])])
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json")])
        launch.assert_not_called()

    def finished_lane_launches_next_batch(self, prs, sweeper=False, board="Stage: Done (@coder)\n"):
        self.occupy_checkout(board)
        if sweeper:
            self.rimz.roster[0].update(handle="sweeper")
            del self.rimz.roster[0]["team"]
            del self.rimz.roster[0]["role"]
        else:
            self.rimz.roster.append(dict(handle="reviewer", team="spot", role="reviewer",
                                        placement=dict(branch="deps/repair-1", pane="tmux:%2", pr=None)))
        occupants = list(self.rimz.roster)
        self.plan = select([source(2)], {2: "failed"})
        events = []
        command = repair.command
        rimz = self.rimz

        def record_command(*args, **kwargs):
            events.append(args[:3])
            return command(*args, **kwargs)

        def record_rimz(*args):
            events.append(args)
            return rimz(*args)

        with self.planned(prs), patch.object(repair, "rimz", side_effect=record_rimz), \
             patch.object(repair, "command", side_effect=record_command), \
             patch.object(repair, "pull_requests", return_value=prs) as pulls, \
             patch.object(repair, "query_plan", return_value=self.plan) as query, \
             patch.object(repair, "launch") as launch:
            self.run_quietly(repair.run)
        launch.assert_called_once_with(self.plan)
        pulls.assert_called_once_with(REPO)
        query.assert_called_once_with(REPO, prs, QUIET_PERIOD)
        stops = [("agents", "stop", agent["placement"]["pane"]) for agent in occupants]
        self.assertEqual([call for call in self.rimz.calls if call[:2] == ("agents", "stop")], stops)
        for stop in stops:
            self.assertLess(events.index(stop), events.index(("git", "worktree", "prune")))
        self.assertEqual(self.lines[:len(occupants)],
                         [dict(stopped=agent["placement"]["pane"], handle=agent["handle"], branch="deps/repair-1",
                               reason="lane finished: board Done, no open PR") for agent in occupants])
        self.assertIn(dict(attempt_checkout="removed", branch="deps/repair-1", reason="clean and pushed"), self.lines)
        self.assertEqual(self.lines[-1], dict(launched="spot#deps-repair-2", branch="deps/repair-2", worktree=str(self.tree("deps/repair-2"))))
        self.assertFalse(self.tree().exists())
        self.assertTrue(self.tree("deps/repair-2").is_dir())

    def test_done_seats_without_a_pr_launch_the_next_batch_in_the_same_fire(self):
        self.finished_lane_launches_next_batch([])

    def test_merged_replacement_sweeper_launches_the_next_batch_in_the_same_fire(self):
        merged = replacement(10, [1], "closed")
        merged["merged_at"] = "2026-10-13T10:00:00Z"
        self.finished_lane_launches_next_batch([merged], sweeper=True)

    def test_closed_unmerged_replacement_seats_launch_the_next_batch_in_the_same_fire(self):
        self.finished_lane_launches_next_batch([replacement(10, [1], "closed")])

    def test_first_stage_line_trimmed_and_without_owner_defines_done(self):
        self.finished_lane_launches_next_batch([], board="# Board\nStage:\t Done \t\nStage: Review\n")

    def test_unfinished_lane_prevents_stopping_other_finished_lanes(self):
        self.occupy_checkout()
        self.rimz.roster.append(dict(handle="other", placement=dict(branch="deps/repair-2", pane="tmux:%2")))
        with self.planned(), patch.object(repair, "launch") as launch:
            self.run_quietly(repair.run)
        self.assertEqual(self.lines, [dict(action="idle", reason="repair lane still occupied", agents=["other"])])
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json")])
        launch.assert_not_called()

    def test_in_use_removal_is_an_idle_wait_before_plan_publish_or_launch(self):
        self.occupy_checkout()
        self.rimz.refusal = "worktree `deps-repair-1` is in use by coder; use --force to remove it"
        with self.planned(), patch.object(repair, "query_plan") as query, \
             patch.object(repair, "open_checkout") as checkout, patch.object(repair, "launch") as launch, \
             patch.object(repair, "prune_empty_batches") as prune, \
             patch.object(repair.sys, "argv", ["dependabot.py", "run"]), redirect_stderr(io.StringIO()):
            self.run_quietly(lambda: self.assertEqual(repair.main(), 0))
        query.assert_not_called()
        checkout.assert_not_called()
        launch.assert_not_called()
        prune.assert_not_called()
        self.assertEqual(self.lines[-2:],
                         [dict(attempt_checkout="kept", branch="deps/repair-1", reason=f"rimz failed: {self.rimz.refusal}"),
                          dict(action="idle", reason="repair lane still closing", branch="deps/repair-1",
                               holder=f"rimz failed: {self.rimz.refusal}")])
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json"),
                                          ("agents", "stop", "tmux:%1"), ("worktree", "remove", "deps/repair-1")])
        self.assertTrue(self.tree().exists())

    def test_failed_stop_fails_before_remove_publish_or_launch(self):
        self.occupy_checkout()
        self.rimz.stop_refusal = "mux close failed"
        with self.planned(), patch.object(repair, "open_checkout") as checkout, \
             patch.object(repair, "launch") as launch, self.assertRaisesRegex(RuntimeError, "mux close failed"):
            self.run_quietly(repair.run)
        checkout.assert_not_called()
        launch.assert_not_called()
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json"), ("agents", "stop", "tmux:%1")])
        self.assertEqual(self.lines, [])
        self.assertTrue(self.tree().exists())

    def test_branch_deleted_on_origin_behind_a_stale_tracking_ref_costs_no_strike(self):
        git(self.root, "push", "--quiet", "origin", "main:refs/heads/deps/repair-7")
        git(self.root, "fetch", "--quiet", "origin")
        git(self.origin, "update-ref", "-d", "refs/heads/deps/repair-7")
        with self.planned(), patch.object(repair, "launch"):
            self.run_quietly(repair.run)
        self.assertFalse(any(line.get("pruned_batch_branch") for line in self.lines))
        self.assertEqual(git(self.root, "for-each-ref", "refs/remotes/origin/deps/repair-7"), "")

    def forge(self, prs):
        endpoints = {"repos/owner/repo/pulls?state=all&per_page=100": prs,
                     "repos/owner/repo/git/matching-refs/heads/deps/repair-": []}
        rollups = {str(pr["number"]): dict(headRefOid=pr["head"]["sha"], statusCheckRollup=[]) for pr in prs}
        return (patch.object(repair, "github", side_effect=lambda *args: REPO if args[0] == "repo" else rollups[args[2]]),
                patch.object(repair, "pages", side_effect=endpoints.__getitem__))

    def test_batch_branch_a_team_left_empty_is_pruned_before_selection(self):
        repair.open_checkout(self.plan)
        self.run_quietly(repair.reclaim_idle_checkouts)
        self.assertNotIn("deps/repair-1", repair.branch_worktrees())
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
        self.assertEqual(self.rimz.calls, [])

    def test_existing_checkout_stops_before_publish_or_create(self):
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-1", str(self.tree()))
        with self.assertRaisesRegex(RuntimeError, "holds unpushed or uncommitted work with no live agent") as error:
            repair.open_checkout(self.plan)
        self.assertEqual(str(error.exception),
                         f"checkout {self.tree()} on deps/repair-1 holds unpushed or uncommitted work with no live agent; "
                         "push it, or discard it with rimz worktree remove deps/repair-1 --force")
        self.assertEqual(self.remote_branch(), "")
        self.assertEqual(self.rimz.calls, [])

    def test_other_repair_checkouts_are_reported_stale_and_left_alone(self):
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-2", str(self.tree("deps/repair-2")))
        report = repair.open_checkout(self.plan)
        self.assertEqual([entry["branch"] for entry in report["stale_attempt_checkouts"]], ["deps/repair-2"])
        self.assertTrue(self.tree("deps/repair-2").is_dir())

    def test_reclaim_removes_every_idle_repair_checkout_unforced(self):
        self.publish_batch()
        repair.open_checkout(self.plan)
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-2", str(self.tree("deps/repair-2")))
        git(self.root, "worktree", "add", "--quiet", "-b", "other", str(self.tree("other")))
        self.rimz.calls.clear()
        self.run_quietly(repair.reclaim_idle_checkouts)
        self.assertEqual(self.rimz.calls, [("worktree", "remove", "deps/repair-1"),
                                          ("worktree", "remove", "deps/repair-2")])
        self.assertEqual(self.lines, [dict(attempt_checkout="removed", branch=f"deps/repair-{n}", reason="clean and pushed")
                                     for n in (1, 2)])
        self.assertFalse(self.tree().exists())
        self.assertFalse(self.tree("deps/repair-2").exists())
        self.assertTrue(self.tree("other").exists())

    def test_refused_reclaim_reports_reason_and_stops_before_publish_or_launch(self):
        git(self.root, "worktree", "add", "--quiet", "-b", "deps/repair-1", str(self.tree()))
        self.rimz.refusal = "worktree `deps-repair-1` has 1 commits not landed on origin/deps/repair-1; run `rimz worktree merge deps-repair-1`, or pass --force to delete them"
        with self.planned(), patch.object(repair, "launch", side_effect=AssertionError("a kept checkout must prevent launch")) as launch, \
             self.assertRaisesRegex(RuntimeError, "push it, or discard it with rimz worktree remove deps/repair-1 --force"):
            self.run_quietly(repair.run)
        launch.assert_not_called()
        self.assertEqual(self.remote_branch(), "")
        self.assertTrue(self.tree().exists())
        self.assertEqual(self.lines[0], dict(attempt_checkout="kept", branch="deps/repair-1",
                                           reason=f"rimz failed: {self.rimz.refusal}"))
        self.assertIn(("worktree", "remove", "deps/repair-1"), self.rimz.calls)
        self.assertTrue((self.root / ".git" / "rimz-dependabot-repair.lock").is_file())

    def test_missing_seat_skills_refuse_before_publish_or_launch(self):
        for role, available, missing in (("coder", ["commit"], "fix-ci"), ("reviewer", [], "pr")):
            with self.subTest(role=role):
                self.rimz.skills = {"coder": ["commit", "fix-ci"], "reviewer": ["pr"]}
                self.rimz.skills[role] = available
                with self.planned(), patch.object(repair, "launch") as launch, \
                     patch.object(repair, "open_checkout", side_effect=AssertionError("missing seat skills must refuse before publishing")), \
                     self.assertRaises(RuntimeError) as error:
                    self.run_quietly(repair.run)
                self.assertEqual(str(error.exception),
                                 f"spot.{role} cannot call required skills {missing}; add them to the profile's skills list")
                launch.assert_not_called()
                self.assertEqual(self.remote_branch(), "")
                self.assertFalse(self.tree().exists())

    def test_seats_without_skill_lists_are_admitted(self):
        self.rimz.skills = {"coder": None, "reviewer": None}

        def check_preflight(*args):
            self.assertEqual([call for call in self.rimz.calls if call[1] == "explain"],
                             [("agents", "explain", "spot.coder", "--json"),
                              ("agents", "explain", "spot.reviewer", "--json")])

        with self.planned(), patch.object(repair, "launch", side_effect=check_preflight) as launch:
            self.run_quietly(repair.run)
        launch.assert_called_once()
        self.assertEqual([call for call in self.rimz.calls if call[1] == "explain"],
                         [("agents", "explain", "spot.coder", "--json"),
                          ("agents", "explain", "spot.reviewer", "--json")])

    def test_failed_launch_fails_the_run_and_leaves_the_checkout(self):
        failure = RuntimeError("rimz teams launch spot -w deps/repair-1 failed (exit 124)")
        with self.planned(), \
             patch.object(repair, "launch", side_effect=failure), \
             patch.object(repair, "github") as github, self.assertRaisesRegex(RuntimeError, "exit 124"):
            self.run_quietly(repair.run)
        github.assert_not_called()
        self.assertEqual([line.get("attempt_checkout") for line in self.lines[1:]], ["created"])
        self.assertTrue(self.tree().exists())

    def test_run_reclaims_before_plan_and_exits_after_one_launch(self):
        self.publish_batch()
        repair.open_checkout(self.plan)
        self.rimz.calls.clear()
        events = []
        command = repair.command

        def record_command(*args, **kwargs):
            events.append(args[:3])
            return command(*args, **kwargs)

        def query(*args):
            self.assertFalse(self.tree().exists())
            events.append(("plan",))
            return self.plan

        def launch(*args):
            self.assertTrue(self.tree().is_dir())
            events.append(("launch",))
            self.assertEqual(args, (self.plan,))
            self.rimz.refusal = "no calls allowed after launch"
            return None

        with self.planned(), patch.object(repair, "command", side_effect=record_command), \
             patch.object(repair, "query_plan", side_effect=query), \
             patch.object(repair, "launch", side_effect=launch) as launched, \
             patch.object(repair.sys, "argv", ["dependabot.py", "run"]), \
             patch.object(repair, "github", side_effect=AssertionError("no GitHub calls after launch")):
            self.run_quietly(lambda: self.assertEqual(repair.main(), 0))
        launched.assert_called_once_with(self.plan)
        self.assertLess(events.index(("git", "worktree", "prune")), events.index(("git", "fetch", "--prune")))
        self.assertLess(events.index(("git", "fetch", "--prune")), events.index(("plan",)))
        self.assertEqual(self.rimz.calls, [("agents", "list", "--all", "--json"),
                                          ("worktree", "remove", "deps/repair-1"),
                                          ("agents", "explain", "spot.coder", "--json"),
                                          ("agents", "explain", "spot.reviewer", "--json"),
                                          ("worktree", "new", "deps/repair-1", "--base", "origin/deps/repair-1")])
        self.assertEqual(self.lines[-1], dict(launched="spot#deps-repair-1", branch="deps/repair-1", worktree=str(self.tree())))
        self.assertTrue(self.tree().exists())


class LaunchTests(unittest.TestCase):
    """The team launch is a hand-off from inside a loop check, and its failures stay readable."""

    PLAN = dict(action="repair", branch="deps/repair-1", sources=[dict(number=1, head_sha="sha-1")])
    LOOP_ENV = {"RIMZ_LOOP_TASK": "dependabot-repair", "RIMZ_WORKSPACE_ID": "ws-1", "RIMZ_PROJECT_ROOT": "/repo"}

    def launch(self, result=None, error=None):
        completed = subprocess.CompletedProcess([], 0, "", "") if result is None else result
        with patch.dict(repair.os.environ, self.LOOP_ENV), \
             patch.object(repair.subprocess, "run", return_value=completed, side_effect=error) as run, \
             redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            try:
                repair.launch(self.PLAN)
            finally:
                self.run_call = run.call_args

    def test_launch_drops_the_loop_marker_and_keeps_the_workspace_pin(self):
        self.launch()
        argv, env = self.run_call.args[0], self.run_call.kwargs["env"]
        self.assertEqual(argv[:4] + argv[5:], ["rimz", "teams", "launch", "spot", "-w", "deps/repair-1", "--bg"])
        self.assertNotIn("RIMZ_LOOP_TASK", env)
        self.assertEqual((env["RIMZ_WORKSPACE_ID"], env["RIMZ_PROJECT_ROOT"]), ("ws-1", "/repo"))
        self.assertEqual(self.run_call.kwargs["stdin"], subprocess.DEVNULL)

    def test_failed_launch_names_the_branch_and_rimz_error_without_the_prompt(self):
        refusal = "a loop check waits for its agent; remove `--bg` to capture the completed run"
        with self.assertRaises(RuntimeError) as raised:
            self.launch(subprocess.CompletedProcess([], 1, "", f"error: {refusal}\n"))
        message = str(raised.exception)
        self.assertEqual(message, f"rimz teams launch spot -w deps/repair-1 failed (exit 1): error: {refusal}")
        self.assertNotIn("Dependency repair plan", message)

    def test_timed_out_launch_omits_the_prompt(self):
        with self.assertRaises(RuntimeError) as raised:
            self.launch(error=subprocess.TimeoutExpired(["rimz", "teams", "launch", "spot", "PROMPT"], 300))
        self.assertEqual(str(raised.exception), "rimz teams launch spot -w deps/repair-1 timed out after 300s")


class RepairTests(unittest.TestCase):
    def test_any_young_open_dependabot_pr_defers_a_fresh_batch(self):
        for state, base, covered in (("failed", "main", False), ("green", "main", False),
                                     ("failed", "release", False), ("failed", "main", True)):
            with self.subTest(state=state, base=base, covered=covered):
                newest = source(2, "2026-10-13T11:59:00Z")
                newest["base"]["ref"] = base
                prs = [newest, source(1)] + ([replacement(10, [2], "closed")] if covered else [])
                plan = select(prs, {1: "failed", 2: state})
                self.assertEqual(plan["action"], "idle")
                self.assertEqual(plan["sources"], [])
                self.assertIsNone(plan["branch"])
                self.assertEqual(plan["reason"],
                                 "Dependabot active: newest open PR #2 created 2026-10-13T11:59:00+00:00; "
                                 "a new batch waits until 2026-10-13T17:59:00+00:00")

    def test_quiet_period_uses_creation_only_and_admits_at_exact_boundary(self):
        for age, action in ((QUIET_PERIOD - timedelta(seconds=1), "idle"),
                            (QUIET_PERIOD, "repair"), (QUIET_PERIOD + timedelta(seconds=1), "repair")):
            with self.subTest(age=age):
                pr = source(1, (NOW - age).isoformat())
                pr["updated_at"] = NOW.isoformat()
                self.assertEqual(select([pr], {1: "failed"})["action"], action)

    def test_closed_or_non_bot_pr_created_a_minute_ago_does_not_defer(self):
        closed = source(2, "2026-10-13T11:59:00Z")
        closed["state"] = "closed"
        human = source(3, closed["created_at"])
        human["user"]["login"] = "maintainer"
        self.assertEqual(select([source(1), closed, human], {1: "failed"})["action"], "repair")

    def test_recent_dependabot_activity_does_not_gate_resume_recover_or_reconcile(self):
        fresh = source(2, "2026-10-13T11:59:00Z")
        prs = [source(1), fresh, replacement(10, [1])]
        resumed = select(prs, {1: "failed", 2: "failed", 10: "failed"})
        self.assertEqual(resumed["reason"], "resume failed replacement on its existing branch")
        recovered = select(prs[:2], {2: "failed"}, ["deps/repair-1"])
        self.assertEqual(recovered["reason"], "recover interrupted batch")
        prs[0]["head"]["sha"] = "changed"
        reconciled = select(prs, {2: "failed", 10: "green"})
        self.assertIn("reconcile", reconciled["reason"])
        for plan in (resumed, recovered, reconciled):
            self.assertEqual((plan["action"], plan["branch"]), ("repair", "deps/repair-1"))

    def test_zero_quiet_period_admits_a_fresh_batch(self):
        self.assertEqual(select([source(1, NOW.isoformat())], {1: "failed"})["action"], "idle")
        self.assertEqual(select([source(1, NOW.isoformat())], {1: "failed"},
                                quiet_period=timedelta(0))["action"], "repair")

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

    def test_launch_passes_prompt_as_data_in_unattended_named_worktree(self):
        plan = select([source(1)], {1: "failed"})
        plan["sources"][0]["title"] = "$(do-not-execute) `neither-this`"
        with patch.object(repair.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as run, \
             redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            repair.launch(plan)
        args, = run.call_args.args
        prompt = (repair.TASK_DIR / "prompt.md").read_text() + "\n\nDependency repair plan (JSON):\n" + json.dumps(plan)
        self.assertEqual(args, ["rimz", "teams", "launch", "spot", prompt, "-w", "deps/repair-1", "--bg"])
        kwargs = {key: value for key, value in run.call_args.kwargs.items() if key != "env"}
        self.assertEqual(kwargs, dict(cwd=repair.ROOT, stdin=subprocess.DEVNULL, text=True, capture_output=True,
                                      timeout=300, check=False))

    def test_cli_passes_quiet_period_to_run_and_plan(self):
        for action in ("run", "plan"):
            for flags, duration in (([], QUIET_PERIOD), (["--quiet-period", "0"], timedelta(0)),
                                    (["--quiet-period", "0.5"], timedelta(minutes=30))):
                with self.subTest(action=action, flags=flags), \
                     patch.object(repair.sys, "argv", ["dependabot.py", action, *flags]), \
                     patch.object(repair, "run") as run, \
                     patch.object(repair, "repo_view", return_value=REPO), \
                     patch.object(repair, "pull_requests", return_value=[]), \
                     patch.object(repair, "query_plan", return_value={}) as query, redirect_stdout(io.StringIO()):
                    self.assertEqual(repair.main(), 0)
                    if action == "run":
                        run.assert_called_once_with(duration)
                        query.assert_not_called()
                    else:
                        query.assert_called_once_with(REPO, [], duration)
                        run.assert_not_called()

    def test_query_plan_supplies_current_time_and_quiet_period_to_select(self):
        clock = unittest.mock.Mock(wraps=datetime)
        clock.now.return_value = NOW
        with patch.object(repair, "datetime", clock), patch.object(repair, "pages", return_value=[]), \
             patch.object(repair, "command", return_value=""), patch.object(repair, "select", return_value={}) as selected:
            repair.query_plan(REPO, [], timedelta(0))
        selected.assert_called_once_with(REPO, [], set(), {}, NOW, timedelta(0))
        clock.now.assert_called_once_with(timezone.utc)


if __name__ == "__main__":
    unittest.main()
