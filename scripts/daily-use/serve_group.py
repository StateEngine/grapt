#!/usr/bin/env python3
"""Private Linux process group for one scratch evidence service.

The sourcing Bash driver owns its lifetime. This helper only launches and stops
that group; it never finds targets by command text or opens a live profile.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def process(pid):
    try:
        with Path(f"/proc/{pid}/stat").open() as stream:
            fields = stream.read(4096).rpartition(")")[2].split()
        return {"state": fields[0], "group": int(fields[2]), "start": fields[19]}
    except (FileNotFoundError, ProcessLookupError):
        return None


def command(pid, limit):
    try:
        with Path(f"/proc/{pid}/cmdline").open("rb") as stream:
            return stream.read(limit)
    except (FileNotFoundError, ProcessLookupError):
        return None


def live_members(group):
    members = []
    with os.scandir("/proc") as entries:
        for index, entry in enumerate(entries):
            if index >= 32768:
                raise RuntimeError("process census exceeded 32768 entries")
            if entry.name.isdecimal():
                value = process(int(entry.name))
                if value and value["group"] == group and value["state"] != "Z":
                    members.append(int(entry.name))
    return members


def stop(record):
    group = record["pid"]
    if not isinstance(group, int) or group <= 1 or group == os.getpgrp():
        raise RuntimeError("refusing an invalid or caller process group")
    leader = process(group)
    if leader and (leader["start"] != record["start"] or leader["group"] != group):
        raise RuntimeError("serve PID no longer identifies its owned process group")
    # A group ID stays reserved while its children live, even after its leader
    # exits. Thus MCP children remain owned when the serve has already died.
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(group, sig)
        except ProcessLookupError:
            return
        for _ in range(50):
            if not live_members(group):
                return
            time.sleep(0.1)
    raise RuntimeError(f"owned serve group {group} did not stop")


def start(args):
    child = None
    published = False
    defer_interruption = True
    pending_signal = None
    # A second driver cannot take over this receipt or the first one's service.
    def interrupted(signum, _frame):
        nonlocal pending_signal
        # Popen may already have spawned the child before returning its handle.
        # Keep that handoff uninterrupted without blocking signals in the child.
        if defer_interruption:
            pending_signal = signum
        else:
            raise InterruptedError(f"launch interrupted by signal {signum}")
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    with args.pidfile.open("x", encoding="utf-8") as receipt:
        try:
            os.chmod(args.pidfile, 0o600)
            if pending_signal is not None:
                raise InterruptedError(f"launch interrupted by signal {pending_signal}")
            with args.log.open("ab") as log:
                child = subprocess.Popen(
                    [str(args.binary), "serve", "--service", str(args.service), "--port", "0"],
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                    start_new_session=True,
                )
            defer_interruption = False
            if pending_signal is not None:
                raise InterruptedError(f"launch interrupted by signal {pending_signal}")
            value = process(child.pid)
            if not value or value["group"] != child.pid:
                raise RuntimeError("serve did not enter its private process group")
            json.dump({"pid": child.pid, "start": value["start"], "owner": args.owner,
                       "service": str(args.service)}, receipt)
            receipt.write("\n")
            receipt.flush()
            published = True
        finally:
            if not published:
                # A repeated signal must not interrupt killing/reaping the
                # unpublished child whose handle we now own.
                defer_interruption = True
                if child:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=5)
                args.pidfile.unlink(missing_ok=True)
    return child


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    launch = sub.add_parser("start")
    launch.add_argument("--binary", type=Path, required=True)
    launch.add_argument("--service", type=Path, required=True)
    launch.add_argument("--log", type=Path, required=True)
    launch.add_argument("--pidfile", type=Path, required=True)
    launch.add_argument("--owner", type=int, required=True)
    finish = sub.add_parser("stop")
    finish.add_argument("--pidfile", type=Path, required=True)
    finish.add_argument("--owner", type=int, required=True)
    inspect = sub.add_parser("inspect")
    inspect.add_argument("--pidfile", type=Path, required=True)
    inspect.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "start":
        args.binary = args.binary.resolve(strict=True)
        args.service = args.service.resolve(strict=True)
        start(args)
    elif args.pidfile.exists():
        with args.pidfile.open(encoding="utf-8") as receipt:
            record = json.loads(receipt.read(4096))
        if args.command == "stop":
            if record["owner"] != args.owner:
                raise RuntimeError("pidfile belongs to a different evidence driver")
            stop(record)
            args.pidfile.unlink()
        else:
            members = []
            for pid in live_members(record["pid"]):
                cmdline = command(pid, 4096)
                if cmdline is not None:
                    argv = cmdline.replace(b"\0", b" ").decode(errors="replace")
                    members.append({"pid": pid, "argv": argv})
            args.output.write_text(json.dumps({"owner": record, "members": members}, indent=2)+"\n")
    elif args.command == "inspect":
        raise RuntimeError("owned serve pidfile is missing")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        raise SystemExit(f"evidence serve: {error}")
