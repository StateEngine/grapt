"""A failed, cancelled or unexpectedly skipped job cannot pass the aggregate."""
import json
import os
import sys


# Gates whose every job always runs: each must be present and succeed.
EVERY_JOB = {
    "--platform": {"archive", "test", "doctest"},
    "--hosted": {"helpers", "clippy", "windows", "archive", "test", "doctest"},
}


def accepted(jobs, *, platform=False, mode=None):
    if platform:
        mode = "--platform"
    if mode is not None:
        return set(jobs) == EVERY_JOB[mode] and all(
            job["result"] == "success" for job in jobs.values())
    required = {
        # `visibility` decides whether this aggregate may publish at all, so its
        # presence is part of the shape: an absent or skipped result means check
        # could not have judged the run, and that is a refusal, never a pass.
        "visibility",
        "changes",
        "linux",
        "clippy",
        "platform",
    }
    if set(jobs) != required:
        return False
    if jobs["visibility"]["result"] != "success":
        return False
    if jobs["changes"]["result"] == "success" and jobs["changes"].get("outputs", {}).get("proven") == "true":
        # Main already passed this exact commit as a batch; nothing else may have run.
        return all(jobs[name]["result"] == "skipped" for name in ("linux", "clippy", "platform"))
    if any(jobs[name]["result"] != "success" for name in ("changes", "linux")):
        return False
    outputs = jobs["changes"].get("outputs", {})
    if any(outputs.get(key) not in {"true", "false"} for key in ("rust", "windows", "hosted")):
        return False
    if outputs["windows"] == "true" and outputs["hosted"] != "true":
        return False
    prose = outputs["rust"] == "false"
    for name in ("clippy", "platform"):
        requested = outputs["windows" if name == "clippy" else "hosted"] == "true"
        expected = "skipped" if prose or not requested else "success"
        if jobs[name]["result"] != expected:
            return False
    return True


if __name__ == "__main__":
    if sys.argv[1:] not in ([], *([mode] for mode in EVERY_JOB)):
        raise SystemExit("Only " + " or ".join(EVERY_JOB) + " is supported")
    jobs = json.loads(os.environ["RESULTS"])
    for name, job in jobs.items():
        print(f"{name}: {job['result']}")
    sys.exit(0 if accepted(jobs, mode=(sys.argv[1:] or [None])[0]) else 1)
