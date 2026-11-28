"""Witnesses for the parts of `compare.py` that decide whether evidence counts.

The `effect` step reads a file OUTSIDE the client and is the only unforgeable
evidence in the J03 journey, so its decision table needs witnesses of its own. A
runner that accepted unreadable bytes as proof of absence would report a green
that means nothing, which is exactly the defect these cover: the first revision
evaluated the absence predicate BEFORE the readability check, and since an
unreadable read yields an empty body, `contains not in ""` was true — corrupt,
oversized and non-UTF-8 evidence all PASSED a marker-absence claim.

Run it directly; it exits non-zero on the first failure.

    python harness-selftest.py
"""

from __future__ import annotations

import importlib.util
import json
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load() -> object:
    spec = importlib.util.spec_from_file_location("compare_under_test", HERE / "compare.py")
    if spec is None or spec.loader is None:
        raise SystemExit("compare.py could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CMP = load()


def load_provider() -> object:
    """The scripted provider, loaded by path because its name is hyphenated."""
    spec = importlib.util.spec_from_file_location(
        "provider_under_test", HERE / "loopback-provider.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("loopback-provider.py could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PROVIDER = load_provider()


def load_service() -> object:
    """The service helper, whose refusal path is witnessed below."""
    spec = importlib.util.spec_from_file_location("service_under_test", HERE / "service.py")
    if spec is None or spec.loader is None:
        raise SystemExit("service.py could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SVC = load_service()

FAILURES: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok   {name}")
        return
    FAILURES.append(f"{name}: {detail}")
    print(f"  FAIL {name}  {detail}")


class FakeObserver:
    """The effect step never touches the client, so this is never called."""

    def state(self):  # pragma: no cover - a call here is itself the defect
        raise AssertionError("the effect step must not read the screen")


def effect(root: Path, wanted: dict, polls: int = 1):
    """Run one effect step, returning ("ok", detail) or ("refused", message)."""
    # Wrapped per client: `resolve` treats ANY dict as a client map, so a bare
    # effect object would be read as "this client declares no effect here" and
    # silently skipped. Every scenario spells it per client for the same reason.
    step = {"effect": {"native": wanted}, "polls": polls}
    try:
        # The producer arguments are unused by every step this file covers —
        # they exist for the `form` step, whose producer is a running product.
        return "ok", CMP.apply_step(FakeObserver(), step, "native", root, root, set())
    except CMP.HarnessError as error:
        return "refused", str(error)


def tls_fixture_server(cert: Path, key: Path) -> tuple[int, threading.Thread, socket.socket]:
    """Start the scripted provider over TLS on an ephemeral port.

    Uses the fixture's OWN `serve_tls`, not a reimplementation, so what this
    witnesses is the shipped harness path rather than a lookalike written for
    the test.
    """
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(str(cert), str(key))
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    port = listener.getsockname()[1]
    served: list[float] = []

    def loop() -> None:
        while True:
            try:
                stream, _ = listener.accept()
            except OSError:
                return
            threading.Thread(
                target=PROVIDER.serve_tls, args=(stream, context, 0, served), daemon=True
            ).start()

    thread = threading.Thread(target=loop, daemon=True)
    thread.start()
    return port, thread, listener


def tls_request(port: int, anchor: Path | None) -> tuple[bool, str]:
    """One provider request over TLS. Returns (handshake_ok, body-or-error).

    `anchor` is the ONLY trust anchor offered. Passing `None` means "trust the
    host's own store", which is the negative case: this fixture's CA is not in
    it and must not be.
    """
    context = ssl.create_default_context(cafile=str(anchor) if anchor else None)
    # The leaf carries `IP:127.0.0.1` and nothing else, so the client must ask
    # for that exact name; anything else would be testing a different claim.
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=10) as raw:
            with context.wrap_socket(raw, server_hostname="127.0.0.1") as secured:
                body = json.dumps(
                    {"input": [{"role": "user", "content": "hello"}]}
                ).encode("utf-8")
                secured.sendall(
                    b"POST /responses HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                    b"Content-Type: application/json\r\n"
                    b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body
                )
                received = bytearray()
                while True:
                    chunk = secured.recv(65536)
                    if not chunk:
                        break
                    received.extend(chunk)
                return True, received.decode("utf-8", "replace")
    except (ssl.SSLError, ssl.SSLCertVerificationError, OSError) as error:
        return False, f"{type(error).__name__}: {error}"


def fixture_tls_witnesses() -> None:
    """The https fixture: the product refuses a plaintext provider base.

    `crates/grapt-provider/src/route.rs` rejects any non-https base at every
    precedence level and offers no switch, on purpose. The fixture therefore
    has to speak TLS for a PACKAGED binary to reach it at all. These witnesses
    cover the fixture end of that — the trust-supply end is Linux-only and is
    proven by a real product run, not here.
    """
    print("fixture TLS")
    if shutil.which("openssl") is None:
        # REFUSE, DO NOT SKIP. A missing tool silently skipping its witnesses is
        # how a suite reports green over a mechanism it never exercised.
        check("openssl is available to build the fixture CA", False, "openssl is not on PATH")
        return
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch) / "ca"
        built = subprocess.run(
            [sys.executable, str(HERE / "fixture-ca.py"), "--root", str(root)],
            capture_output=True, text=True,
        )
        check("fixture-ca.py builds a CA and leaf", built.returncode == 0,
              built.stderr.strip()[:400])
        if built.returncode != 0:
            return
        check("it refuses a second run over a non-empty root",
              subprocess.run(
                  [sys.executable, str(HERE / "fixture-ca.py"), "--root", str(root)],
                  capture_output=True, text=True,
              ).returncode != 0,
              "a silent overwrite would leave listener and client trusting different CAs")

        port, _, listener = tls_fixture_server(root / "server.crt", root / "server.key")
        try:
            handshook, body = tls_request(port, root / "ca.crt")
            check("a client trusting the fixture CA completes the handshake", handshook, body[:200])
            check("the scripted turn streams over TLS",
                  handshook and "text/event-stream" in body and "response.output_text.delta" in body,
                  body[:200])
            # The chunked terminator is what makes the end of the body a fact
            # rather than a disconnection; the plaintext path learned that the
            # hard way and the TLS path must not lose it.
            check("the response is chunk-framed to a clean end",
                  handshook and body.rstrip().endswith("0"),
                  repr(body[-40:]))

            # THE NEGATIVE THAT MAKES THE POSITIVE MEAN SOMETHING. Without it,
            # every assertion above would hold just as well against a fixture
            # still serving plaintext to a client that never verified anything.
            untrusted, detail = tls_request(port, None)
            check("a client not trusting the fixture CA is REFUSED",
                  not untrusted and "CERTIFICATE_VERIFY_FAILED" in detail,
                  detail[:200])
        finally:
            listener.close()


def durable_end_state_witnesses() -> None:
    """The B3 gate: a green comparison must not survive a recorded failure.

    A review measured four sessions ending in `session.execution.failed` while
    the run reported `failures: [null, null]`. These witness the gate that now
    catches it — including the negative case, because a check that cannot fail
    is not a check.
    """
    import sqlite3

    print("the durable end state is read, and a failure there is caught:")
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        database = root / "database.sqlite3"
        connection = sqlite3.connect(database)
        connection.execute(
            "CREATE TABLE event (seq INTEGER, aggregate_family TEXT, aggregate_id TEXT, type TEXT)"
        )
        connection.execute(
            "INSERT INTO event VALUES (1, 'session', 'ses_clean', 'session.execution.succeeded.1')"
        )
        connection.commit()

        clean = CMP.execution_failures(root, ["ses_clean"])
        check("a session with no failure reads clean", clean["observed"] and not clean["failed"])

        # INJECTED. The same store, one failed execution added.
        connection.execute(
            "INSERT INTO event VALUES (2, 'session', 'ses_broken', 'session.execution.failed.1')"
        )
        connection.commit()
        connection.close()

        caught = CMP.execution_failures(root, ["ses_clean", "ses_broken"])
        check(
            "an injected execution failure is caught, and named",
            caught["observed"] and caught["failed"] == {"ses_broken": 1},
        )

    missing = CMP.execution_failures(Path(raw), ["ses_clean"])
    check(
        "a store that cannot be read yields NO verdict rather than a clean one",
        not missing["observed"] and "reason" in missing,
    )
    unbound = CMP.execution_failures(Path(raw), None)
    check("sessions that could not be bound yield no verdict either", not unbound["observed"])


def store_authority_witnesses() -> None:
    """The store-authority ladder, driven through the FILE-BACKED launch path.

    B3 moved the product's streams from pipes to files. That fixed the
    undrained-pipe failures and broke the refusal path in the same stroke:
    `communicate()` on a file-backed child returns `(None, None)`, so
    `stdout + stderr` raised TypeError and the `migrate -> open-ready` rung
    below it became unreachable. Measured for real during a canonical
    post-land restart, against a store the product had correctly refused.

    These drive `serve()` against a scripted stand-in for the product that
    refuses exactly as it does, so the ladder is exercised rather than argued.
    """
    import os
    import stat
    import subprocess

    print("the store-authority ladder survives file-backed output:")

    # A stand-in that refuses while the config says `migrate`, and serves once
    # it says `open-ready` -- the product's own rule, and nothing else.
    script = '''
import json, sys
from pathlib import Path
root = Path(sys.argv[sys.argv.index("--service") + 1])
authority = json.loads((root / "grapt.jsonc").read_text(encoding="utf-8"))["store"]["authority"]
if authority != "open-ready":
    print("store directive Migrate does not authorize first-contact state Ready", file=sys.stderr)
    raise SystemExit(1)
(root / "serve-ready.json").write_text(json.dumps({"pid": 1, "ok": True}), encoding="utf-8")
# Serve until told to stop. A sentinel rather than a kill, because on Windows
# this runs under a .cmd shim: killing the shim leaves THIS process alive,
# still holding the inherited log handles, and the root then cannot be deleted.
import time
for _ in range(600):
    if (root / "stop").exists():
        break
    time.sleep(0.05)
'''

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        (root / "grapt.jsonc").write_text(
            json.dumps({"store": {"authority": "migrate"}}), encoding="utf-8"
        )
        source = root / "stand-in.py"
        source.write_text(script, encoding="utf-8")
        if os.name == "nt":
            binary = root / "stand-in.cmd"
            binary.write_text(f'@"{sys.executable}" "{source}" %*\n', encoding="utf-8")
        else:
            binary = root / "stand-in.sh"
            binary.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{source}" "$@"\n', encoding="utf-8")
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)

        # THE REFUSAL IS READ, not crashed on. This is the regression itself:
        # before the correction this call raised TypeError from `None + None`.
        refusal = SVC.launch(root, binary)
        check(
            "a refusing product returns its diagnostic rather than raising",
            isinstance(refusal, str),
            f"got {type(refusal).__name__}",
        )
        check(
            "and the diagnostic carries the product's own stated reason",
            isinstance(refusal, str) and SVC.MIGRATED in refusal,
            f"refusal={refusal!r}"[:300],
        )

        # The whole ladder, end to end: refuse, advance, serve.
        child = SVC.serve(root, binary)
        try:
            check(
                "serve() climbs to open-ready and returns a live child",
                isinstance(child, subprocess.Popen) and child.poll() is None,
            )
            check(
                "the config was advanced to open-ready, and only that",
                json.loads((root / "grapt.jsonc").read_text(encoding="utf-8"))
                == {"store": {"authority": "open-ready"}},
            )
            check(
                "the refusal is PRESERVED, not truncated by the relaunch",
                (root / "product-refusal-migrate.log").is_file()
                and SVC.MIGRATED
                in (root / "product-refusal-migrate.log").read_text(encoding="utf-8"),
            )
        finally:
            (root / "stop").write_text("", encoding="utf-8")
            try:
                child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()

    # ABSENT AND UNREADABLE ARE NOT THE SAME AS SILENT, and none of the three is
    # an empty string -- a refusal nobody can read must not be matched against
    # MIGRATED as though it had been read.
    with tempfile.TemporaryDirectory() as raw:
        empty = Path(raw)
        absent = SVC.diagnostic(empty)
        check(
            "a product that wrote nothing reports both streams ABSENT",
            absent.count("<absent>") == 2,
            f"diagnostic={absent!r}",
        )
        (empty / "product-stdout.log").write_text("", encoding="utf-8")
        (empty / "product-stderr.log").write_text("refused for a reason", encoding="utf-8")
        mixed = SVC.diagnostic(empty)
        check(
            "an empty stream and a written one are distinguished",
            "<empty>" in mixed and "refused for a reason" in mixed,
            f"diagnostic={mixed!r}",
        )
        check(
            "and a diagnostic is never silently empty",
            SVC.diagnostic(empty) != "" and absent != "",
        )


def undrained_pipe_witnesses() -> None:
    """The class found in three more helpers, witnessed once.

    `compare.py`, `journey.py` and `reconnect-run.py` each launched a child that
    LIVES FOR THE WHOLE RUN with `stdout=PIPE, stderr=PIPE` and nothing draining
    them. That is B3's shape: the buffer fills — 4 KB on Windows — and the next
    write blocks inside the child. For the observer that stalls the journey; for
    the relay it is a CUT the scenario never asked for, which would read as the
    client failing to reconnect.

    Two properties, and the second is the one a reader should not have to take
    on faith: a verbose child must actually SURVIVE writing more than any pipe
    buffer would hold.
    """
    import subprocess

    print("a long-lived child's output never fills an undrained pipe:")

    # 1 MiB, far past any pipe buffer, written by a child nobody reads.
    volume = 1024 * 1024
    script = (
        "import sys\n"
        f"sys.stdout.write('o' * {volume})\n"
        "sys.stdout.flush()\n"
        f"sys.stderr.write('the child refused: NAMED REASON' + 'e' * {volume})\n"
        "sys.stderr.flush()\n"
        "raise SystemExit(3)\n"
    )

    with tempfile.TemporaryDirectory() as raw:
        room = Path(raw)
        source = room / "verbose.py"
        source.write_text(script, encoding="utf-8")

        out = (room / "child-stdout.log").open("w", encoding="utf-8")
        err = (room / "child-stderr.log").open("w", encoding="utf-8")
        child = subprocess.Popen([sys.executable, str(source)], stdout=out, stderr=err, text=True)
        out.close()
        err.close()
        try:
            code = child.wait(timeout=60)
            finished = True
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            code, finished = None, False

        check(
            "a child writing 1 MiB to each stream EXITS instead of blocking",
            finished and code == 3,
            f"finished={finished} code={code!r}",
        )
        check(
            "and every byte it wrote is retained, not truncated to a buffer",
            (room / "child-stdout.log").stat().st_size >= volume
            and (room / "child-stderr.log").stat().st_size >= volume,
        )

        # THE REFUSAL TEXT REACHES THE CALLER. This is what the three helpers
        # raise, so a run that stops names the reason the child gave.
        reported = CMP.child_diagnostic(room, "child")
        check(
            "the caller's diagnostic carries the child's own stated reason",
            "the child refused: NAMED REASON" in reported,
        )
        check(
            "and it is bounded rather than pasting a megabyte into an exception",
            len(reported) <= 2 * CMP.DIAGNOSTIC_BYTES + 256,
            f"len={len(reported)}",
        )

    # The three helpers carry the same function; they must agree, or a reader
    # who checks one has learned nothing about the other two.
    import importlib.util

    for name in ("journey.py", "reconnect-run.py"):
        spec = importlib.util.spec_from_file_location(f"sibling_{name}", HERE / name)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            room = Path(raw)
            (room / "x-stderr.log").write_text("NAMED REASON", encoding="utf-8")
            text = module.child_diagnostic(room, "x")
            check(
                f"{name} reports a present stream and an absent one distinctly",
                "NAMED REASON" in text and "<absent>" in text,
                f"text={text!r}",
            )


def capture_coherence_witnesses() -> None:
    print("a capture the observer called incoherent is not evidence:")
    settled = [
        {"kind": "capture", "detail": {"label": "settled", "coherent": True}},
        {"kind": "key", "detail": "Enter"},
    ]
    check("a coherent capture passes", not CMP.incoherent_captures(settled))
    mid_render = settled + [
        {"kind": "capture", "detail": {"label": "the child's answer", "coherent": False}}
    ]
    check(
        "an incoherent capture is caught, and named by its caption",
        CMP.incoherent_captures(mid_render) == ["the child's answer"],
    )




def provider_shape_witnesses() -> None:
    """The scripted provider's declared instruction shapes, and its turn anchor.

    THE ARGUMENTS ARE ASSERTED AGAINST THE PRODUCT'S SCHEMA, not against what
    the provider happens to emit. `edit` requires `old_text` as well as
    `new_text`, and a call missing one is refused before any authority ask is
    drawn — which the scenario would report as a wait that timed out, naming
    the screen rather than the cause.

    THE ANCHOR WITNESS IS THE ONE THAT MATTERS. Continuation used to be decided
    by scanning the whole transcript, so a session that made one tool call
    could never make another. The J03 approval scenario drives three, and the
    run failed with its first tool fully green — the shape of a defect no
    single-tool scenario can expose.
    """
    print("the scripted provider's declared shapes and turn anchoring:")
    check(
        "the shell shape yields the command the product's schema requires",
        PROVIDER.shell_command(
            "Use the shell tool to run exactly this command and nothing else: echo A> j03.txt"
        )
        == "echo A> j03.txt",
    )
    check(
        "the read shape yields a bare path",
        PROVIDER.read_target(
            "Use the read tool to read j03.txt and tell me the single word it contains."
        )
        == {"path": "j03.txt"},
    )
    check(
        "the edit shape yields BOTH the old text and the new one",
        PROVIDER.edit_change(
            "Use the edit tool to change the word in j03.txt from ALPHAMARKER to BETAMARKER."
        )
        == {"path": "j03.txt", "old_text": "ALPHAMARKER", "new_text": "BETAMARKER"},
    )
    check(
        "an instruction naming no declared shape asks for no tool",
        PROVIDER.read_target("Say hello") is None
        and PROVIDER.edit_change("Say hello") is None
        and PROVIDER.shell_command("Say hello") is None,
    )

    def request(rows: list) -> bytes:
        return json.dumps({"input": rows}).encode("utf-8")

    user = {"role": "user", "content": "go"}
    call = {"type": "function_call", "name": "shell", "call_id": "call_j04_shell"}
    result = {"type": "function_call_output", "call_id": "call_j04_shell", "output": "ok"}
    check(
        "a first turn is not a continuation",
        not PROVIDER.has_tool_result(request([user])),
    )
    check(
        "a turn whose last row is a tool result IS a continuation, so no tool loop",
        PROVIDER.has_tool_result(request([user, call, result])),
    )
    check(
        "a NEW user turn after a completed call is NOT a continuation",
        not PROVIDER.has_tool_result(
            request([user, call, result, {"role": "user", "content": "now read it"}])
        ),
    )


def call_identity_witnesses() -> None:
    """The fixture's call ids must tell two calls apart, and stay put within one.

    WHY THIS EXISTS, measured on a real run rather than imagined. Run 06 ARM B
    failed TWICE, after permission was granted and before any elicitation could
    raise a form, with a 76-byte captured payload reading:

        Session("call call_j04_journey_journey_echo has already reached a terminal")

    The product refused it correctly: an MCP connection keeps a SETTLED LEDGER
    and `begin_call` rejects an id that already reached a terminal
    (`grapt-mcp/src/session.rs:318`), an invariant that exists to make duplicate
    terminals impossible. The fixture was handing it the SAME id twice, because
    the mint was `f"call_j04_{tool}"` — derived from the tool name alone.

    `next_call_id` already fixed that here. **Nothing tested it**, which is how a
    correction ships and then fails to reach the copy a run actually uses. These
    witnesses are the missing half.

    The identity must be: unique across distinct calls on a
    surviving connection, stable across the frames of one call, and DETERMINISTIC
    fixture-owned — not random, not time-derived, and never by weakening the
    product's ledger.
    """
    print()
    print("call identity distinguishes calls without weakening the product:")

    PROVIDER._call_sequence = 0
    PROVIDER._repeat_call_ids = False

    first = PROVIDER.next_call_id("journey_journey_echo")
    second = PROVIDER.next_call_id("journey_journey_echo")
    check(
        "TWO CALLS to the same tool get DISTINCT ids",
        first != second,
        f"{first!r} == {second!r} — this is the run 06 ARM B failure",
    )
    check(
        "and both still name the tool, so a reader can see what was called",
        "journey_journey_echo" in first and "journey_journey_echo" in second,
        f"{first!r} {second!r}",
    )

    # DETERMINISTIC: the same sequence from the same start, every process.
    PROVIDER._call_sequence = 0
    check(
        "the sequence is DETERMINISTIC, not random or time-derived",
        PROVIDER.next_call_id("journey_journey_echo") == first,
        "a fixture whose ids vary run to run cannot be asserted against",
    )

    # STABLE WITHIN ONE CALL: every frame of one call carries one id.
    PROVIDER._call_sequence = 0
    frames = PROVIDER.tool_call_frames({"text": "x"}, 0.0, "journey_journey_echo")
    # Each frame is a (payload, delay) tuple; the id must be one value across
    # every payload that names it, in either the call_id or the item_id form.
    seen = set()
    for payload, _delay in frames:
        for found in re.findall(r'"call_id"\s*:\s*"([^"]+)"', payload):
            seen.add(found)
        for found in re.findall(r'"item_id"\s*:\s*"([^"]+)"', payload):
            seen.add(found.removeprefix("item_"))
        for found in re.findall(r'"id"\s*:\s*"item_([^"]+)"', payload):
            seen.add(found)
    check(
        "WITHIN-CALL frames all carry the SAME id",
        len(seen) == 1,
        f"frames disagreed: {sorted(seen)}",
    )
    check(
        "and the frames really did carry an id, so the row is not vacuous",
        len(seen) == 1 and next(iter(seen)).startswith("call_j04_"),
        f"no id found in {len(frames)} frames: {sorted(seen)}",
    )

    # THE CALL SITE, not just the mint. Found by mutation: reverting
    # `call = next_call_id(tool)` to the flat `f"call_j04_{tool}"` left every row
    # above GREEN — the within-call row passes either way, because a flat id is
    # also "the same across frames". Covering the function is not covering the
    # fact that the caller uses it, which is the run 06 defect exactly.
    PROVIDER._call_sequence = 0
    def frame_id(payload_frames):
        found = set()
        for payload, _delay in payload_frames:
            for value in re.findall(r'"call_id"\s*:\s*"([^"]+)"', payload):
                found.add(value)
        return found

    first_call = frame_id(PROVIDER.tool_call_frames({"text": "x"}, 0.0, "journey_journey_echo"))
    second_call = frame_id(PROVIDER.tool_call_frames({"text": "x"}, 0.0, "journey_journey_echo"))
    check(
        "TWO SUCCESSIVE tool_call_frames for one tool EMIT DIFFERENT ids",
        first_call and second_call and first_call != second_call,
        f"{sorted(first_call)} vs {sorted(second_call)} — this is run 06 ARM B",
    )

    # THE NEGATIVE CORPUS stays reproducible, on purpose.
    PROVIDER._repeat_call_ids = True
    flat_one = PROVIDER.next_call_id("journey_journey_echo")
    flat_two = PROVIDER.next_call_id("journey_journey_echo")
    check(
        "--repeat-call-ids REPRODUCES the pre-correction collision on purpose",
        flat_one == flat_two == "call_j04_journey_journey_echo",
        f"{flat_one!r} {flat_two!r}",
    )
    check(
        "and that flat id is exactly the one the product refused in run 06",
        flat_one == "call_j04_journey_journey_echo",
        flat_one,
    )
    PROVIDER._repeat_call_ids = False
    PROVIDER._call_sequence = 0


def t5_budget_witnesses() -> None:
    passing = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [
                    {"kind": "type", "t5_echo": True, "elapsed_ms": 8.0, "streaming_observed": True, "observed_tok_per_s": 240.0},
                    {"kind": "key", "t5_echo": True, "elapsed_ms": 12.5, "streaming_observed": True, "observed_tok_per_s": 220.0},
                    {"kind": "echo", "elapsed_ms": 15.9, "streaming_observed": True, "observed_tok_per_s": 210.0},
                ],
                "idle_cpu_seconds": 0.12,
            }
        ]
    )
    check(
        "t5 smoothness budgets pass within 16ms echo p95, witnessed >=200 tok/s streaming, and idle CPU ceiling",
        passing["status"] == "pass"
        and passing["echo_p95_ms"]["status"] == "pass"
        and passing["concurrent_streaming_tok_per_s"]["status"] == "pass"
        and passing["idle_cpu_seconds"]["status"] == "pass",
        f"got={passing!r}",
    )
    static_rate_only = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [
                    {"kind": "echo", "elapsed_ms": 8.0, "streaming_tok_per_s": 250.0},
                ],
                "idle_cpu_seconds": 0.12,
            }
        ]
    )
    check(
        "t5 smoothness budgets refuse pass when streaming rate is only statically declared (unwitnessed)",
        static_rate_only["status"] == "not-measured"
        and static_rate_only["concurrent_streaming_tok_per_s"]["status"] == "not-measured",
        f"got={static_rate_only!r}",
    )
    skipped_echo = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [
                    {"kind": "echo", "t5_echo": True, "elapsed_ms": 8.0, "streaming_observed": True, "observed_tok_per_s": 240.0},
                    {"kind": "echo", "t5_echo": True, "skipped": True, "elapsed_ms": None, "detail": {"skipped": "native skipped"}},
                ],
                "idle_cpu_seconds": 0.10,
            }
        ]
    )
    check(
        "t5 smoothness budgets fail closed when any tagged echo step is skipped by native",
        skipped_echo["status"] == "fail"
        and skipped_echo["echo_p95_ms"]["status"] == "fail"
        and skipped_echo["echo_p95_ms"].get("reason") == "t5-echo-step-skipped",
        f"got={skipped_echo!r}",
    )
    failing = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [{"kind": "echo", "elapsed_ms": 22.0}],
                "t5_metrics": {"idle_cpu_seconds": 0.8},
            }
        ]
    )
    check(
        "t5 smoothness budgets fail when echo p95 or idle CPU exceeds budget",
        failing["status"] == "fail"
        and failing["echo_p95_ms"]["status"] == "fail"
        and failing["idle_cpu_seconds"]["status"] == "fail",
        f"got={failing!r}",
    )
    missing_echo = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [{"kind": "type", "seconds": 0.01}],
                "idle_cpu_seconds": 0.05,
            }
        ]
    )
    check(
        "t5 smoothness budgets refuse pass when echo samples are absent",
        missing_echo["status"] == "not-measured"
        and missing_echo["echo_p95_ms"]["status"] == "not-measured",
        f"got={missing_echo!r}",
    )
    cumulative_only = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [{"kind": "echo", "elapsed_ms": 5.0}],
                "resources": {"processor_seconds": 0.05},
            }
        ]
    )
    check(
        "t5 smoothness budgets never treat cumulative journey CPU as idle CPU",
        cumulative_only["status"] == "not-measured"
        and cumulative_only["idle_cpu_seconds"]["status"] == "not-measured",
        f"got={cumulative_only!r}",
    )
    unmeasured = CMP.evaluate_t5_budgets([{"client": "retained", "steps": []}])
    check(
        "t5 smoothness budgets report not-measured when native side is absent",
        unmeasured["status"] == "not-measured",
        f"got={unmeasured!r}",
    )
    readings = iter(
        [
            {"observed": True, "processor_seconds": 1.25},
            {"observed": True, "processor_seconds": 1.29},
        ]
    )
    delta = CMP.measure_idle_cpu_delta(1234, 0.001, sampler=lambda _pid: next(readings))
    check(
        "measure_idle_cpu_delta computes quiet window delta rather than cumulative CPU",
        delta == 0.04,
        f"got={delta!r}",
    )
    unobserved = CMP.measure_idle_cpu_delta(
        1234, 0.001, sampler=lambda _pid: {"observed": False, "processor_seconds": None}
    )
    check(
        "measure_idle_cpu_delta returns None when process CPU is unobserved",
        unobserved is None,
        f"got={unobserved!r}",
    )
    decreased_readings = iter(
        [
            {"observed": True, "processor_seconds": 2.50},
            {"observed": True, "processor_seconds": 1.10},
        ]
    )
    decreased = CMP.measure_idle_cpu_delta(
        1234, 0.001, sampler=lambda _pid: next(decreased_readings)
    )
    check(
        "measure_idle_cpu_delta fails closed (None) instead of clamping decreased counters to zero",
        decreased is None,
        f"got={decreased!r}",
    )
    partial_counters_readings = iter(
        [
            {"observed": True, "processor_seconds": 1.25, "processes_without_counters": 0},
            {"observed": True, "processor_seconds": 1.29, "processes_without_counters": 1},
        ]
    )
    partial_counters = CMP.measure_idle_cpu_delta(
        1234, 0.001, sampler=lambda _pid: next(partial_counters_readings)
    )
    check(
        "measure_idle_cpu_delta fails closed on partial process-tree counter coverage",
        partial_counters is None,
        f"got={partial_counters!r}",
    )
    check(
        "is_quit_step identifies quit/Control+q and never treats Control+c interrupt as quit",
        CMP.is_quit_step({"key": "Control+q"}, "native")
        and CMP.is_quit_step({"quit": True}, "native")
        and not CMP.is_quit_step({"key": "Control+c"}, "native")
        and not CMP.is_quit_step({"key": "Ctrl+c"}, "native")
        and not CMP.is_quit_step({"key": "Enter"}, "native"),
    )

    class _FakeObserver:
        def __init__(self, screens: list[str], stream_tokens: list[int | None] | None = None) -> None:
            self._screens = list(screens)
            self._tokens = list(stream_tokens) if stream_tokens is not None else [None] * len(screens)
            self._index = 0

        def state(self) -> dict:
            idx = min(self._index, len(self._screens) - 1)
            payload: dict[str, object] = {"lines": [self._screens[idx]]}
            if self._tokens[idx] is not None:
                payload["stream_tokens"] = self._tokens[idx]
            return payload

        def action(self, _payload: dict) -> None:
            self._index += 1

    already_present_rejected = False
    try:
        CMP.apply_step(
            _FakeObserver(["prompt ECHO_MARKER already here"]),
            {"echo": "ECHO_MARKER"},
            "native",
            Path("."),
            Path("."),
            set(),
        )
    except CMP.HarnessError:
        already_present_rejected = True
    check(
        "echo step refuses when marker was already present on screen before input",
        already_present_rejected,
    )
    # Negative runner test: a large static status repaint or pasted composer
    # text (without genuine assistant stream_tokens) must NOT satisfy T5 streaming.
    repaint_kind, repaint_detail, _ = CMP.apply_step(
        _FakeObserver(["prompt > ", f"{'STATUS_BAR_REPAINT ' * 80}prompt > REPAINT_ECHO"]),
        {"echo": "REPAINT_ECHO"},
        "native",
        Path("."),
        Path("."),
        set(),
    )
    check(
        "echo step succeeds when marker was absent before input and newly painted after",
        repaint_kind == "echo"
        and isinstance(repaint_detail, dict)
        and repaint_detail.get("observed") is True
        and repaint_detail.get("absent_before") is True
        and repaint_detail.get("streaming_observed") is False
        and repaint_detail.get("echo_ms") is not None,
        f"got={repaint_detail!r}",
    )
    repaint_budget = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [
                    {
                        "kind": repaint_kind,
                        "t5_echo": True,
                        "elapsed_ms": repaint_detail["echo_ms"],
                        "detail": repaint_detail,
                    }
                ],
                "idle_cpu_seconds": 0.10,
            }
        ]
    )
    check(
        "static screen repaint or pasted composer text never satisfies T5 concurrent streaming",
        repaint_budget["status"] == "not-measured"
        and repaint_budget["concurrent_streaming_tok_per_s"]["status"] == "not-measured",
        f"got={repaint_budget!r}",
    )
    # Positive end-to-end runner test: genuine observer-reported assistant
    # stream_tokens advancing alongside the echo marker populates
    # streaming_observed=True and observed_tok_per_s >= 200 so T5 passes.
    stream_kind, stream_detail, _ = CMP.apply_step(
        _FakeObserver(
            ["prompt > ", "assistant streaming... prompt > STREAM_ECHO"],
            stream_tokens=[10, 60],
        ),
        {"echo": "STREAM_ECHO"},
        "native",
        Path("."),
        Path("."),
        set(),
    )
    runner_budget = CMP.evaluate_t5_budgets(
        [
            {
                "client": "native",
                "steps": [
                    {
                        "kind": stream_kind,
                        "t5_echo": True,
                        "elapsed_ms": stream_detail["echo_ms"],
                        "streaming_observed": stream_detail["streaming_observed"],
                        "observed_tok_per_s": stream_detail["observed_tok_per_s"],
                        "detail": stream_detail,
                    }
                ],
                "idle_cpu_seconds": 0.10,
            }
        ]
    )
    check(
        "runner echo with genuine stream_tokens populates observed_tok_per_s and passes T5 budgets",
        stream_detail.get("streaming_observed") is True
        and float(stream_detail.get("observed_tok_per_s", 0.0)) >= 200.0
        and runner_budget["status"] == "pass"
        and runner_budget["concurrent_streaming_tok_per_s"]["status"] == "pass",
        f"detail={stream_detail!r} budget={runner_budget!r}",
    )
    # End-to-end loopback-provider witness: real loopback-provider.py streaming
    # SSE deltas updates the stream-metrics file while an observer with NO
    # stream_tokens field in /state runs apply_step -> evaluate_t5_budgets passes.
    with tempfile.TemporaryDirectory() as raw_stream_dir:
        metrics_path = Path(raw_stream_dir) / "provider.stream-tokens.json"
        PROVIDER._write_stream_metrics(metrics_path, 0)
        client_sock, server_sock = socket.socketpair()
        served_times: list[float] = []
        stream_counter = {"tokens": 0}
        stream_lock = threading.Lock()
        request_body = json.dumps(
            {"input": [{"role": "user", "content": "stream a reply"}]}
        ).encode("utf-8")
        request_wire = (
            f"POST /responses HTTP/1.1\r\nHost: 127.0.0.1\r\n"
            f"Content-Length: {len(request_body)}\r\n\r\n"
        ).encode("utf-8") + request_body

        class _StreamingObserver:
            def __init__(self) -> None:
                self._sent = False

            def state(self) -> dict:
                # Deliberately no `stream_tokens` field in /state: compare.py
                # must witness real streaming from loopback-provider's metrics file.
                return {"lines": ["prompt > LOOPBACK_ECHO" if self._sent else "prompt > "]}

            def action(self, _payload: dict) -> None:
                client_sock.sendall(request_wire)
                client_sock.shutdown(socket.SHUT_WR)
                worker = threading.Thread(
                    target=PROVIDER.serve,
                    args=(server_sock, 0, served_times, metrics_path, stream_counter, stream_lock),
                )
                worker.start()
                while client_sock.recv(4096):
                    pass
                worker.join(timeout=5)
                self._sent = True

        try:
            lb_kind, lb_detail, _ = CMP.apply_step(
                _StreamingObserver(),
                {"echo": "LOOPBACK_ECHO"},
                "native",
                Path("."),
                Path("."),
                set(),
                stream_metrics_path=metrics_path,
            )
        finally:
            client_sock.close()
        lb_budget = CMP.evaluate_t5_budgets(
            [
                {
                    "client": "native",
                    "steps": [
                        {
                            "kind": lb_kind,
                            "t5_echo": True,
                            "elapsed_ms": lb_detail["echo_ms"],
                            "streaming_observed": lb_detail["streaming_observed"],
                            "observed_tok_per_s": lb_detail["observed_tok_per_s"],
                            "detail": lb_detail,
                        }
                    ],
                    "idle_cpu_seconds": 0.10,
                }
            ]
        )
        check(
            "end-to-end loopback-provider stream-metrics witness passes T5 budgets without /state stream_tokens",
            lb_detail.get("streaming_observed") is True
            and float(lb_detail.get("observed_tok_per_s", 0.0)) >= 200.0
            and lb_budget["status"] == "pass"
            and lb_budget["concurrent_streaming_tok_per_s"]["status"] == "pass",
            f"detail={lb_detail!r} budget={lb_budget!r}",
        )
    non_t5_rejected = False
    try:
        CMP.validate_t5_scenario({"name": "j02"}, [{"key": "Enter"}])
    except CMP.HarnessError:
        non_t5_rejected = True
    check(
        "validate_t5_scenario rejects non-T5 scenarios under --enforce-t5-budgets",
        non_t5_rejected,
    )


def main() -> int:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)

        # ---- read_effect states, each distinct ---------------------------
        print("read_effect distinguishes every way evidence can fail:")
        state, _ = CMP.read_effect(root / "nope.txt", 4096)
        check("missing is missing", state == "missing", state)

        good = root / "good.txt"
        good.write_text("ALPHAMARKER", encoding="utf-8")
        state, body = CMP.read_effect(good, 4096)
        check("readable is read", state == "read" and body == "ALPHAMARKER", f"{state} {body!r}")

        big = root / "big.txt"
        big.write_bytes(b"x" * 50)
        state, _ = CMP.read_effect(big, 10)
        check("over the limit is oversized", state == "oversized", state)

        check(
            "exactly at the limit still reads",
            CMP.read_effect(big, 50)[0] == "read",
            CMP.read_effect(big, 50)[0],
        )

        bad = root / "bad.txt"
        bad.write_bytes(b"\xff\xfe\x00rubbish")
        state, _ = CMP.read_effect(bad, 4096)
        check("invalid utf-8 is invalid, not repaired", state == "invalid", state)

        # A directory where a file is expected: an OSError that is not
        # FileNotFoundError, which must not be reported as absence.
        (root / "adir").mkdir()
        state, _ = CMP.read_effect(root / "adir", 4096)
        check("an unreadable path is not absence", state == "unreadable", state)

        # ---- THE CORRECTION: unreadable evidence proves nothing ----------
        print("unreadable evidence is refused in BOTH directions:")
        for name, path in (("oversized", "big.txt"), ("invalid", "bad.txt"), ("unreadable", "adir")):
            limit = 10 if name == "oversized" else 4096
            outcome, _ = effect(
                root, {"path": path, "contains": "ALPHAMARKER", "absent": True, "limit": limit}
            )
            check(f"{name} cannot satisfy marker-absence", outcome == "refused", outcome)

            outcome, _ = effect(
                root, {"path": path, "contains": "ALPHAMARKER", "missing": True, "limit": limit}
            )
            check(f"{name} cannot satisfy file-absence", outcome == "refused", outcome)

            outcome, _ = effect(
                root, {"path": path, "contains": "ALPHAMARKER", "limit": limit}
            )
            check(f"{name} cannot satisfy presence", outcome == "refused", outcome)

        # ---- missing / readable-old / readable-new -----------------------
        print("the three honest states are told apart:")
        outcome, _ = effect(root, {"path": "nope.txt", "contains": "NEW", "absent": True})
        check("missing satisfies marker-absence", outcome == "ok", outcome)
        outcome, _ = effect(root, {"path": "nope.txt", "missing": True})
        check("missing satisfies file-absence", outcome == "ok", outcome)
        outcome, _ = effect(root, {"path": "nope.txt", "contains": "NEW"})
        check("missing cannot satisfy presence", outcome == "refused", outcome)

        old = root / "old.txt"
        old.write_text("ALPHAMARKER", encoding="utf-8")
        outcome, _ = effect(root, {"path": "old.txt", "contains": "BETAMARKER", "absent": True})
        check("old bytes satisfy absence OF THE NEW MARKER", outcome == "ok", outcome)
        outcome, _ = effect(root, {"path": "old.txt", "missing": True})
        check(
            "but old bytes do NOT satisfy 'nothing was written'",
            outcome == "refused",
            outcome,
        )
        outcome, _ = effect(root, {"path": "old.txt", "contains": "BETAMARKER"})
        check("and do not satisfy presence of the new marker", outcome == "refused", outcome)

        old.write_text("BETAMARKER", encoding="utf-8")
        outcome, _ = effect(root, {"path": "old.txt", "contains": "BETAMARKER"})
        check("new bytes satisfy presence", outcome == "ok", outcome)
        outcome, _ = effect(root, {"path": "old.txt", "contains": "BETAMARKER", "absent": True})
        check("and no longer satisfy absence", outcome == "refused", outcome)

        # ---- containment --------------------------------------------------
        print("a path may not leave the client's own project:")
        outcome, message = effect(root, {"path": "../escape.txt", "contains": "X"})
        check("an escaping path is refused", outcome == "refused", message[:60])

    print("a session binding is never assembled from a half-read store:")
    created, error = CMP.sessions_created(None, {"ses_a", "ses_b"})
    check(
        "a failed BEFORE read cannot attribute preexisting sessions",
        created is None and error is not None,
        f"created={created!r}",
    )
    created, error = CMP.sessions_created({"ses_a"}, None)
    check(
        "a failed AFTER read yields no binding either",
        created is None and error is not None,
        f"created={created!r}",
    )
    created, error = CMP.sessions_created(None, None)
    check("neither read yields no binding", created is None and error is not None, f"{created!r}")
    created, error = CMP.sessions_created({"ses_a"}, {"ses_a", "ses_b"})
    check(
        "two good reads name only what appeared",
        created == ["ses_b"] and error is None,
        f"created={created!r} error={error!r}",
    )
    created, error = CMP.sessions_created(set(), set())
    check(
        "an empty store before and after is a real empty binding",
        created == [] and error is None,
        f"created={created!r}",
    )

    fixture_tls_witnesses()
    print()
    durable_end_state_witnesses()
    print()
    store_authority_witnesses()
    print()
    undrained_pipe_witnesses()
    print()
    capture_coherence_witnesses()
    print()
    t5_budget_witnesses()
    print()
    provider_shape_witnesses()
    call_identity_witnesses()

    print()
    if FAILURES:
        print(f"{len(FAILURES)} failure(s)")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print("all harness witnesses pass")
    return 0


if __name__ == "__main__":
    sys.exit(main())
