"""Private IPC adapter for the existing Rust witness; never print/store input or raw replies."""
import argparse
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time


def run(args):
    if not args.recipient or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', args.recipient):
        raise ValueError('set STATEENGINE_AGENT_ID or pass --recipient for the bound identity')
    private = sys.stdin.buffer.read(8193)
    if len(private) > 8192:
        raise ValueError("input bound")
    pinned = json.loads(args.profile.read_text())["limits"]
    if args.proof_line:
        if not private.endswith(b"\n") or private.count(b"\n") != 1:
            raise ValueError("proof frame")
        proof = private[:-1].decode("utf-8")
        if not proof or any(char.isspace() for char in proof):
            raise ValueError("proof frame")
        if not args.client:
            raise ValueError("client missing")
        packet = {"port": 4877, "capability": proof, "source_project": "secode",
                  "target_project": "secode", "recipient": args.recipient,
                  "database": str(args.owned.resolve(strict=True) / "mail.sqlite"),
                  "client": args.client, "limits": pinned}
        if args.transport_protocol != "remote-seat-mail-transport/v1":
            packet["transport_protocol"] = args.transport_protocol
        if args.witness_message:
            if len(args.witness_message) > 8 or any(not value or len(value.encode()) > 256 for value in args.witness_message):
                raise ValueError("witness selector bound")
            packet["witness_messages"] = args.witness_message
        private = json.dumps(packet).encode("utf-8")
        if len(private) > 8192:
            raise ValueError("packet bound")
    else:
        packet = json.loads(private)
    fields = {"port", "capability", "source_project", "target_project", "recipient", "database", "client", "limits"}
    if not fields.issubset(packet) or set(packet) - fields - {"witness_messages", "transport_protocol"} or packet["port"] != 4877:
        raise ValueError("input shape")
    transport_protocol = packet.get("transport_protocol", "remote-seat-mail-transport/v1")
    if transport_protocol != args.transport_protocol:
        raise ValueError("transport context")
    selected_ids = packet.get("witness_messages")
    if selected_ids is not None and (not isinstance(selected_ids, list) or not 1 <= len(selected_ids) <= 8
                                    or any(not isinstance(value, str) or not value or len(value.encode()) > 256 for value in selected_ids)):
        raise ValueError("witness selector bound")
    if packet["source_project"] != "secode" or packet["target_project"] != "secode" or packet["recipient"] != args.recipient:
        raise ValueError("principal")
    if not isinstance(packet["capability"], str) or not 1 <= len(packet["capability"].encode()) <= 8192:
        raise ValueError("private input")
    capability = packet["capability"]
    if packet["limits"] != pinned:
        raise ValueError("profile")
    # The broker chooses a fresh witness-owned DB, never a user's store.
    owned = args.owned.resolve(strict=True)
    if args.owned.is_symlink() or owned.stat().st_uid != os.getuid() or owned.stat().st_mode & 0o077:
        raise ValueError("owned root")
    if Path(packet["database"]) != owned / "mail.sqlite" or (owned / "mail.sqlite").is_symlink():
        raise ValueError("owned database")
    child = subprocess.Popen([str(args.probe)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    result = None
    try:
        child.stdin.write(private)
        child.stdin.close()
        private = None
        packet = None
        output = bytearray()
        diagnostic = bytearray()
        error_bytes = 0
        deadline = time.monotonic() + 60
        with selectors.DefaultSelector() as selected:
            selected.register(child.stdout, selectors.EVENT_READ, "out")
            selected.register(child.stderr, selectors.EVENT_READ, "err")
            while selected.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("consumer deadline")
                for key, _ in selected.select(min(remaining, 1)):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selected.unregister(key.fileobj)
                    elif key.data == "out":
                        output.extend(data)
                        if len(output) > 16 * 1024 * 1024:
                            raise ValueError("output bound")
                    else:
                        error_bytes += len(data)
                        if error_bytes > 8192:
                            raise ValueError("diagnostic bound")
                        diagnostic.extend(data)
            status = child.wait(timeout=max(0.01, deadline - time.monotonic()))
        if status:
            # Rust diagnostics and received bodies remain private, including on refusal.
            # Only closed source error names leave this private buffer, never its text.
            known = ("Unauthenticated", "ForeignProjectMismatch", "ExactPrincipalMismatch",
                     "RecipientMappingMissing", "RecipientMismatch", "ProtocolVersionUnsupported",
                     "TransportUnavailable", "SubscriptionRefused", "MalformedFrame",
                     "MalformedEnvelope", "UnknownRequiredField", "Deadline", "ForeignBinding",
                     "ForeignReceipt", "StoreUnavailable", "SliceTooSmall", "NotLive",
                     "RecoveryUnavailable", "CursorDidNotAdvance")
            hints = [name for name in known if re.search(rb"\b" + name.encode() + rb"\b", diagnostic)]
            return {"state": "REFUSED", "consumer_exit": status,
                    "consumer_diagnostic_bytes": error_bytes, "diagnostic_hints": hints,
                    "private_input_bytes": len(capability.encode()),
                    "source_close": "not established by output"}
        reply = json.loads(output)
        output.clear()
        receipt_fields = {"live", "rows", "shutdown"}
        if transport_protocol == "remote-seat-mail-transport/v2":
            receipt_fields |= {"integrity_gaps", "transport_protocol"}
            if reply.get("transport_protocol") != transport_protocol or type(reply.get("integrity_gaps")) is not int or not 0 <= reply["integrity_gaps"] <= 8192:
                raise ValueError("v2 progress receipt")
        if set(reply) not in (receipt_fields, receipt_fields | {"admitted_count"}) or reply["live"] is not True or reply["shutdown"] is not True:
            raise ValueError("receipt shape")
        rows = reply["rows"]
        if not isinstance(rows, list) or len(rows) > 64:
            raise ValueError("row bound")
        if selected_ids is not None and any(not any(row["source_message"] == value for row in rows) for value in selected_ids):
            raise ValueError("required witness missing")
        facts = [{"source_message": row["source_message"], "local_admission": row["local_admission"],
                  "actionable": row["actionable"], "delivery_pending": row["delivery_pending"],
                  "repair_pending": row["repair_pending"], "body_utf8_bytes": len(row["body"].encode())} for row in rows]
        result = {"state": "LIVE", "source_close": "joined", "rows": facts, "real_paid_provider_calls": 0}
        if transport_protocol == "remote-seat-mail-transport/v2":
            result["transport_protocol"] = transport_protocol
            result["integrity_gaps"] = reply["integrity_gaps"]
        if "admitted_count" in reply:
            count = reply["admitted_count"]
            if type(count) is not int or not len(rows) <= count <= 8192:
                raise ValueError("admission count bound")
            result["admitted_count"] = count
        for fact, row in zip(facts, rows):
            if "source_project" in row:
                if not isinstance(row["source_project"], str) or not 1 <= len(row["source_project"].encode()) <= 256:
                    raise ValueError("source project bound")
                fact["source_project"] = row["source_project"]
        if capability in json.dumps(result):
            raise ValueError("private input reflected in receipt")
        return result
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        for stream in (child.stdin, child.stdout, child.stderr):
            if stream:
                stream.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recipient", default=os.environ.get("STATEENGINE_AGENT_ID"))
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--profile", required=True, type=Path)
    parser.add_argument("--owned", required=True, type=Path)
    parser.add_argument("--proof-line", action="store_true", help="Accept the supported bridge's private newline frame")
    parser.add_argument("--client", help="Public client identity for proof-line ingress")
    parser.add_argument("--transport-protocol", choices=["remote-seat-mail-transport/v1", "remote-seat-mail-transport/v2"], default="remote-seat-mail-transport/v1", help="Explicit transport selection; no fallback")
    parser.add_argument("--witness-message", action="append", help="Public id required in the bounded selected readout")
    arguments = parser.parse_args()
    try:
        receipt = run(arguments)
    except Exception as error:
        # Class only: exceptions can contain private input, so never render their text.
        receipt = {"state": "REFUSED", "failure_class": type(error).__name__, "source_close": "unconfirmed"}
    print(json.dumps(receipt))
    sys.exit(0 if receipt["state"] == "LIVE" else 1)
