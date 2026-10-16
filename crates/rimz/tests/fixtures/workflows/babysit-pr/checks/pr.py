# /// script
# requires-python = ">=3.11"
# ///
"""Read the branch's PR; a dead attempt backs off without consuming a visit."""

import datetime
import json
import os
import pathlib
import subprocess

context = json.loads((pathlib.Path(os.environ["RIMZ_WORKFLOW_DIR"]) / "context.json").read_text())
previous = context.get("prev_attempt") or {}
now = datetime.datetime.now(datetime.UTC)
recheck = None
if previous.get("outcome") == "died" and previous.get("ended_at"):
    recheck = datetime.datetime.fromisoformat(previous["ended_at"]) + datetime.timedelta(minutes=10)
elif previous.get("outcome") == "limited" and (previous.get("limited") or {}).get("reset_at"):
    recheck = datetime.datetime.fromisoformat(previous["limited"]["reset_at"])
if recheck and recheck > now:
    result = {"outcome": "ci-pending", "reason": "The previous attempt died; wait ten minutes before retrying.", "recheck_at": recheck.isoformat()}
    if previous["outcome"] == "limited":
        result["reason"] = "Wait for provider capacity."
else:
    pr = json.loads(subprocess.check_output(["gh", "pr", "view", "--json", "state,statusCheckRollup"], text=True))
    pending = any(check.get("status") in {"QUEUED", "IN_PROGRESS", "PENDING", "WAITING", "REQUESTED"} or check.get("state") in {"PENDING", "EXPECTED"} for check in pr.get("statusCheckRollup") or [])
    outcome = "merged" if pr["state"] == "MERGED" else "ci-pending" if pending else "needs-sweep"
    result = {"outcome": outcome}
    if pending:
        result["recheck_at"] = (datetime.datetime.now(datetime.UTC) + datetime.timedelta(minutes=10)).isoformat()
print(json.dumps(result))
