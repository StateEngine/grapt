"""A scripted provider on loopback, so a comparative run costs no model spend.

## Why this exists

J04 compares two clients on identical journeys, and one of the declared
scenarios is streaming. A real provider makes that real — and makes every run
billable, non-deterministic in latency, and impossible to repeat exactly. The
product already has the seam for this: under its named testkit feature,
`GRAPT_LIVE_HEADLESS_LOOPBACK` points the production provider path at a
loopback endpoint, so the transport, the streaming decode, the durable capture
and both clients' rendering are all the real ones. Only the model is replaced.

That is the same seam the headless product witnesses use. It is NOT a client
double: neither client knows anything about it, and neither is configured
differently because of it.

## What it serves

The response frames are the provider's own event shapes, emitted with a delay
between deltas so streaming is something an observer can actually watch happen
rather than a single frame that arrives complete. The delay is declared here
and recorded in the receipt, because a comparison whose pacing is unknown
cannot be read as a comparison of anything.

Usage:

    python loopback-provider.py --port 0 --delay-ms 60 --receipt <file>
"""

from __future__ import annotations

import argparse
import json
import socket
import ssl
import sys
import threading
import time
from pathlib import Path

READ_BYTES = 64 * 1024
# Bounded: a request larger than the product's own prompt bound is not one this
# script should be trying to answer, and reading without a ceiling would make a
# runaway client a memory fault here rather than a refusal.
MAX_REQUEST_BYTES = 8 * 1024 * 1024

# The echo ceiling. A large paste is one of the compared scenarios, and a
# provider that streamed all of it back would make this script the thing under
# test rather than the clients.
MAX_ECHO_CHARS = 400

# HOW THIS FIXTURE MINTS TOOL CALL IDS.
#
# It used to be `f"call_j04_{tool}"` — derived from the TOOL NAME alone, so two
# journeys calling the same tool were handed the SAME id. That flattened a real
# discriminating identity: a provider mints a fresh id per call, and the product
# treats `ToolCallId` as an identity it records durably. The delivered J03 corpus
# therefore contains two dispatches indistinguishable by call id, which read as a
# product limitation and was in fact this line.
#
# The replacement is a MONOTONIC PER-PROCESS COUNTER, not a random or
# time-derived token. A fixture whose ids vary run to run cannot be asserted
# against, and ambient randomness would make every captured transcript a
# one-off; a counter gives the same sequence every run while still telling two
# admitted calls apart, which is exactly what a witness needs.
#
# `--repeat-call-ids` restores the old flat behaviour ON PURPOSE. The repeated-id
# corpus is the NEGATIVE CASE for the correlation work — proof that a call id
# alone cannot bind a form to a session — so it has to stay reproducible rather
# than become an artefact nobody can regenerate.
CALL_ID_PREFIX = "call_j04"
_call_sequence = 0
_repeat_call_ids = False


def next_call_id(tool: str) -> str:
    """Deterministic, and distinct per admitted call unless told otherwise."""
    global _call_sequence
    if _repeat_call_ids:
        return f"{CALL_ID_PREFIX}_{tool}"
    _call_sequence += 1
    return f"{CALL_ID_PREFIX}_{_call_sequence:04d}_{tool}"

TEXT = (
    "Yes. The native client and the retained client are both attached to this "
    "product, and this answer is streaming through the same provider transport "
    "to each of them, one fragment at a time, exactly as a model's would."
)


def frames(delay_ms: int, prompt: str, continuing: bool = False) -> list:
    """The scripted turn, as (SSE frame, delay before it) pairs."""
    delay = delay_ms / 1000.0
    # `slowly` is a declared instruction too, and it is load-bearing rather than
    # decorative: the scenario that asks for a slow answer is the one that then
    # INTERRUPTS it, and an answer that finishes first leaves nothing to
    # interrupt. Measured: a faster fragmentation shortened that window and the
    # retained client's interrupt assertion began failing on a client that had
    # not changed at all.
    if "slowly" in prompt.lower():
        delay *= SLOW_FACTOR
    # Checked FIRST because the journey prompt that asks for it also contains
    # ordinary words; the existing shapes stay exactly as they were and none of
    # them matches this marker, so their behaviour is byte-identical.
    echo = mcp_echo_request(prompt) if not continuing else None
    if echo is not None:
        return tool_call_frames(echo, delay, MCP_ECHO_TOOL)
    task = delegated_task(prompt) if not continuing else None
    if task is not None:
        return tool_call_frames({"task": task}, delay, "subagent")
    command = shell_command(prompt) if not continuing else None
    if command is not None:
        return tool_call_frames({"command": command}, delay, "shell")
    read = read_target(prompt) if not continuing else None
    if read is not None:
        return tool_call_frames(read, delay, "read")
    edit = edit_change(prompt) if not continuing else None
    if edit is not None:
        return tool_call_frames(edit, delay, "edit")
    # J04 injects failure AT THE PROVIDER-STREAM BOUNDARY. Both modes are keyed
    # on explicit `rq-` tokens no existing declared scenario uses, so every
    # caller that does not ask for them keeps byte-identical behaviour. They are
    # declared here rather than produced by killing the fixture mid-flight,
    # because a scenario that races a teardown measures the race, not the
    # product.
    lowered = prompt.lower()
    if "rq-malformed" in lowered:
        # A frame the client CANNOT parse, then a clean close. The product must
        # report a truthful failure; anything that settles this as a completed
        # answer is inventing content the provider never sent.
        return [
            ('data: {"type":"response.reasoning_summary_text.delta",'
             '"delta":"Considering the comparison."}\n\n', 0.0),
            ('data: {"type":"response.output_text.delta","delta":"partial ans', delay),
            ('data: {"type":"response.output_text.delta","delta":\n\n', delay),
        ]
    if "rq-truncated" in lowered:
        # Well-formed frames that simply STOP: no `response.completed` ever
        # arrives. This is the stream-interruption boundary, and it is the one a
        # product is most likely to paper over by settling on end-of-stream.
        return [
            ('data: {"type":"response.reasoning_summary_text.delta",'
             '"delta":"Considering the comparison."}\n\n', 0.0),
            ('data: {"type":"response.reasoning_summary_text.done"}\n\n', delay),
            ('data: {"type":"response.output_text.delta","delta":"partial"}\n\n', delay),
        ]

    spoken = answer_to(prompt)
    out: list[tuple[str, float]] = [
        (
            'data: {"type":"response.reasoning_summary_text.delta",'
            '"delta":"Considering the comparison."}\n\n',
            0.0,
        ),
        ('data: {"type":"response.reasoning_summary_text.done"}\n\n', delay),
    ]
    for fragment in fragments_of(spoken):
        out.append(
            (
                "data: "
                + json.dumps({"type": "response.output_text.delta", "delta": fragment})
                + "\n\n",
                delay,
            )
        )
    out.append(('data: {"type":"response.output_text.done"}\n\n', delay))
    out.append(
        (
            'data: {"type":"response.completed","response":{"usage":{"input_tokens":18,'
            '"output_tokens":31,"total_tokens":49,"input_tokens_details":'
            '{"cached_tokens":0,"cache_write_tokens":0},'
            '"output_tokens_details":{"reasoning_tokens":4}}}}\n\n',
            delay,
        )
    )
    return out


# Spelled numbers a declared scenario uses. Deliberately tiny: this is not a
# language model and must not pretend to be one, so an unrecognised spelling is
# answered the ordinary way rather than guessed at.
SPELLED = {
    "ten": 10,
    "fifty": 50,
    "one hundred": 100,
    "two hundred": 200,
    "three hundred": 300,
    "five hundred": 500,
}

# The most fragments one answer is streamed in. A 300-line answer sent one line
# per frame at the declared delay would take longer than the scenario's own
# bounded wait, and the pacing -- not either client -- would be what the run
# measured.
MAX_FRAGMENTS = 40

# How much longer a fragment takes when the prompt asks for a SLOW answer.
# The name the MODEL is shown, which is NOT the name the MCP server publishes.
#
# I had these confused, and the product says so plainly:
# `crates/grapt-product/src/dogfood/mcp/publication.rs` mints the published name
# as `{namespace(server)}_{tool}` and notes the frozen oracle does the same at
# `packages/core/src/tool/mcp.ts:20-21` — and that the SAME qualified name is the
# permission ACTION, so the name a model is shown, the name a user approves and
# the name a receipt records are one string.
#
# The MCP server publishes `journey_echo`; the recipe names the server `journey`;
# so the model-facing name is `journey_journey_echo`. Emitting the MCP-side name
# produced a call the product could not route — "Tool failed", with no
# `tools/call` ever reaching the server. Measuring the transcript gave me the
# server's name and I read it as the model's.
MCP_ECHO_SERVER = "journey"
MCP_ECHO_TOOL = f"{MCP_ECHO_SERVER}_journey_echo"
SLOW_FACTOR = 4


def delegated_task(prompt: str) -> "str | None":
    """The declared delegation instruction, or `None`.

    `Delegate: <task>` is how a scenario asks for a real subagent turn — the
    parent calls the product's `subagent` tool, an authority ask appears, a
    child Session runs its own turn through this same provider, and its answer
    comes back as the tool result. Every part of that is the product's; this
    only decides that the model asked for it.
    """
    marker = "delegate:"
    lowered = prompt.lower()
    if marker not in lowered:
        return None
    task = prompt[lowered.index(marker) + len(marker) :].strip()
    return task[:MAX_ECHO_CHARS] if task else None


def mcp_echo_request(prompt: str) -> "dict | None":
    """The declared MCP instruction, or `None`.

    `Call journey_echo with text <value>` is how the J03 MCP form dimension asks
    for a real tool call through the product's own MCP path. Without it the
    fixture simply answers in prose, no tool call is emitted, no permission is
    raised, no form is elicited, and the accepted desktop journey times out
    waiting for a decision that was never going to appear.

    THE NAME AND ARGUMENT SHAPE ARE MEASURED, NOT GUESSED. A real transcript from
    the accepted dogfood run publishes the tool as `journey_echo` with an input
    schema whose only property is `text`, and calls it as
    {"name": "journey_echo", "arguments": {"text": ...}}. This emits that exact
    shape so the product's MCP path does the eliciting; nothing here fabricates a
    form, a reply or a settlement.
    """
    marker = "call journey_echo with text "
    lowered = prompt.lower()
    if marker not in lowered:
        return None
    tail = prompt[lowered.index(marker) + len(marker):].strip()
    # The declared value ends at the first sentence break; the journey prompt
    # continues with further instructions after it.
    for stop in (".", chr(10)):
        if stop in tail:
            tail = tail.split(stop, 1)[0]
    tail = tail.strip()
    return {"text": tail} if tail else None


def shell_command(prompt: str) -> "str | None":
    """The declared shell instruction, or `None`.

    `Use the shell tool to run exactly this command and nothing else: <cmd>` is
    how the J03 approval and refusal scenarios ask for a real authority
    decision. Honouring it here is what lets those two dimensions run against a
    live retained client without model spend — which matters because the
    retained client marks a REFUSED tool with strikethrough rather than with
    different words, so the only way to check that claim is to watch it.
    """
    marker = "use the shell tool to run exactly this command and nothing else:"
    lowered = prompt.lower()
    if marker not in lowered:
        return None
    command = prompt[lowered.index(marker) + len(marker) :].strip()
    return command[:MAX_ECHO_CHARS] if command else None


def read_target(prompt: str) -> "dict | None":
    """The declared read instruction, or `None`.

    `Use the read tool to read <path> and tell me the single word it contains.`
    A read has no on-disk effect, so the J03 scenario's evidence for it is the
    settled row each client draws. That row cannot exist unless a real read
    call is made against production authority, which is what this asks for.
    """
    marker = "use the read tool to read "
    lowered = prompt.lower()
    if marker not in lowered:
        return None
    rest = prompt[lowered.index(marker) + len(marker) :].strip()
    path = rest.split()[0].strip() if rest.split() else ""
    return {"path": path[:MAX_ECHO_CHARS]} if path else None


def edit_change(prompt: str) -> "dict | None":
    """The declared edit instruction, or `None`.

    `Use the edit tool to change the word in <path> from <old> to <new>.` The
    product's edit schema requires the OLD text as well as the new one, so both
    are named in the instruction rather than left for a model to infer — this
    provider has no file access and could not infer them, and a scenario that
    depended on inference would be measuring the model instead of the clients.
    """
    marker = "use the edit tool to change the word in "
    lowered = prompt.lower()
    if marker not in lowered:
        return None
    rest = prompt[lowered.index(marker) + len(marker) :].strip()
    words = rest.rstrip(".").split()
    if len(words) < 5 or words[1].lower() != "from" or words[3].lower() != "to":
        return None
    path, old_text, new_text = words[0], words[2], words[4]
    if not path or not old_text or not new_text:
        return None
    return {
        "path": path[:MAX_ECHO_CHARS],
        "old_text": old_text[:MAX_ECHO_CHARS],
        "new_text": new_text[:MAX_ECHO_CHARS],
    }


def tool_call_frames(argument_object: dict, delay: float, tool: str) -> list:
    """One tool call, in the provider's own function-call event shape.

    The arguments are passed in already shaped for the named tool rather than
    derived here from the tool's name: each declared instruction shape knows
    which fields the product's schema requires, and a builder that guessed
    would be a second place for that knowledge to drift.
    """
    call = next_call_id(tool)
    arguments = json.dumps(json.dumps(argument_object))
    return [
        (
            "data: "
            + json.dumps(
                {
                    "type": "response.output_item.added",
                    "item": {
                        "type": "function_call",
                        "id": f"item_{call}",
                        "call_id": call,
                        "name": tool,
                    },
                }
            )
            + chr(10) * 2,
            0.0,
        ),
        (
            'data: {"type":"response.function_call_arguments.delta","item_id":"item_'
            + call
            + '","delta":'
            + arguments
            + "}"
            + chr(10) * 2,
            delay,
        ),
        (
            'data: {"type":"response.function_call_arguments.done","item_id":"item_'
            + call
            + '"}'
            + chr(10) * 2,
            0.0,
        ),
        (
            'data: {"type":"response.completed","response":{"usage":'
            '{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}' + chr(10) * 2,
            0.0,
        ),
    ]


def counting_answer(prompt: str) -> "str | None":
    """The declared counting instruction, or `None` when it is not one.

    `count from one to <n>, one number per line` is how the large-history
    scenario asks for a transcript long enough to scroll. It is answered here
    rather than by a model for the same reason as every other frame in this
    file: the clients, the transport and the store are the real ones, and the
    model is the only thing replaced.
    """
    lowered = prompt.lower()
    if "count from one to" not in lowered:
        return None
    tail = lowered[lowered.index("count from one to") + len("count from one to") :].strip()
    upper = None
    for spelling, value in SPELLED.items():
        if tail.startswith(spelling):
            upper = value
            break
    if upper is None:
        digits = ""
        for character in tail:
            if character.isdigit():
                digits += character
            elif digits:
                break
        upper = int(digits) if digits and len(digits) <= 4 else None
    if upper is None or upper <= 0:
        return None
    lines = []
    marker = "reply with the word "
    if marker in lowered:
        word = prompt[lowered.index(marker) + len(marker) :].split()
        if word:
            lines.append(word[0].strip(".,"))
    lines.extend(str(number) for number in range(1, upper + 1))
    return chr(10).join(lines)


def answer_to(prompt: str) -> str:
    """What this scripted provider says back.

    Two shapes, both of them about making a turn IDENTIFIABLE on screen:

    - `Reply with exactly: <text>` is answered with exactly that text. The
      declared scenarios use that instruction to give a turn a canary they can
      then look for, and a provider that ignored it would make the canary
      unreachable — measured: both clients streamed a full answer and the
      native side's `[text] comparative canary` line could never appear,
      because the answer began with the echoed instruction instead.
    - anything else is answered with the prompt echoed back, bounded, followed
      by the standing text. The echo is what lets a scenario without an
      explicit instruction still tell its own turn from the previous one.

    Obeying one declared instruction shape is not the script understanding the
    prompt. It is the fixture equivalent of a model that follows an
    instruction, and it is spelled out here so a reader of the receipt knows
    exactly what produced the words on the screen.
    """
    counted = counting_answer(prompt)
    if counted is not None:
        return counted
    marker = "reply with exactly:"
    lowered = prompt.lower()
    if marker in lowered:
        tail = prompt[lowered.index(marker) + len(marker) :].strip()
        # To the end of the sentence, so trailing scenario instructions such as
        # "Do not use tools." are not spoken as part of the canary.
        cut = tail.find(".")
        exact = (tail[:cut] if cut >= 0 else tail).strip()
        if exact:
            return exact[:MAX_ECHO_CHARS]
    if prompt:
        return prompt[:MAX_ECHO_CHARS] + " " + TEXT
    return TEXT


def fragments_of(spoken: str) -> list:
    """Split an answer into at most `MAX_FRAGMENTS` streamed pieces.

    Word by word for a short answer, so streaming is visibly incremental; in
    equal blocks for a long one, so a three-hundred-line transcript does not
    take longer to emit than the scenario is willing to wait. Newlines survive
    either way -- the large-history scenario asserts on lines.
    """
    # A LINE-SHAPED answer is split on lines, not on spaces. Counting to three
    # hundred has no spaces in it at all, so a word split emitted the whole
    # transcript in ONE frame and streamed nothing.
    newline = chr(10)
    separator = newline if newline in spoken else " "
    pieces = spoken.split(separator)
    if len(pieces) <= MAX_FRAGMENTS:
        return [piece + separator for piece in pieces]
    size = (len(pieces) + MAX_FRAGMENTS - 1) // MAX_FRAGMENTS
    return [
        separator.join(pieces[start : start + size]) + separator
        for start in range(0, len(pieces), size)
    ]


def read_request(stream: socket.socket, *, diagnostics: bool = True) -> tuple:
    """Consume one HTTP request, bounded, and return the prompt it carries.

    The prompt is ECHOED into the answer. A scenario proves a turn arrived by
    putting a canary in what it types and looking for it on screen, and a
    provider that always said the same thing makes that assertion unprovable —
    as measured: both clients streamed a full answer and neither could show the
    canary. Echoing keeps the assertion meaningful with no model spend.
    """
    buffered = b""
    while b"\r\n\r\n" not in buffered:
        chunk = stream.recv(READ_BYTES)
        if not chunk:
            return "", False
        buffered += chunk
        if len(buffered) > MAX_REQUEST_BYTES:
            raise RuntimeError("provider request exceeded its bound")
    head, _, rest = buffered.partition(b"\r\n\r\n")
    length = 0
    for line in head.decode("latin-1").split("\r\n"):
        name, _, value = line.partition(":")
        if name.strip().lower() == "content-length":
            length = int(value.strip() or 0)
    body = rest
    while len(body) < length:
        chunk = stream.recv(READ_BYTES)
        if not chunk:
            break
        body += chunk
        if len(body) > MAX_REQUEST_BYTES:
            raise RuntimeError("provider request body exceeded its bound")
    # DIAGNOSTIC, names only. The MODEL-FACING tool name is not necessarily the
    # MCP-side name: the MCP server publishes `journey_echo`, and the product may
    # advertise it to the provider under a namespaced name. Emitting the names it
    # actually offers is the only way to know what a declared scenario must say.
    try:
        offered = json.loads(body.decode("utf-8")).get("tools") or []
        names = [t.get("name") or (t.get("function") or {}).get("name") for t in offered]
        if diagnostics:
            print("TOOLS_OFFERED " + json.dumps([n for n in names if n]), flush=True)
    except Exception:
        pass
    return prompt_of(body), has_tool_result(body)


def has_tool_result(body: bytes) -> bool:
    """Is this request a CONTINUATION of the turn the LAST user message began?

    It decides whether a tool-asking turn asks for its tool again. The parent's
    continuation still contains the user's original words, so keying only on
    those would make every such turn loop: call the tool, receive the result,
    read the same instruction, call it again.

    IT MUST BE ANCHORED TO THE LAST USER TURN, and that is a correction from a
    measured run rather than a refinement. This scanned the WHOLE input list,
    so a session became permanently `continuing` the moment it made ONE tool
    call: every later request still carried that call in its history. The J03
    approval scenario drives THREE tools in one session, and it failed with the
    shell half fully green and the read ask never appearing at all — the
    provider had answered in words because a call from two turns earlier was
    still in the transcript. A conversation prefix is not a statement about the
    turn now being asked for; only what follows the last user message is.
    """
    try:
        decoded = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return False
    rows = decoded.get("input")
    if not isinstance(rows, list):
        return False
    for row in reversed(rows):
        if not isinstance(row, dict):
            continue
        if str(row.get("type", "")).startswith("function_call"):
            return True
        if row.get("role") == "user":
            return False
    return False


def prompt_of(body: bytes) -> str:
    """The last user turn in a provider request, or empty when there is none."""
    try:
        decoded = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return ""
    rows = decoded.get("input")
    if not isinstance(rows, list):
        return ""
    for row in reversed(rows):
        if not isinstance(row, dict) or row.get("role") != "user":
            continue
        content = row.get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            for part in content:
                if isinstance(part, dict) and isinstance(part.get("text"), str):
                    return part["text"]
    return ""


def _extract_stream_scope(headers: dict[str, str], body: bytes) -> str | None:
    """Identify which terminal/session owns this provider stream so traffic on
    another socket can never satisfy a quiet T5 echo probe."""
    for header in ("x-grapt-stream-scope", "x-grapt-session-id"):
        value = headers.get(header, "").strip()
        if value:
            return value
    try:
        decoded = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(decoded, dict):
        return None
    for key in ("stream_scope", "prompt_cache_key"):
        value = decoded.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    metadata = decoded.get("metadata")
    if isinstance(metadata, dict):
        session_id = metadata.get("session_id")
        if isinstance(session_id, str) and session_id.strip():
            return session_id.strip()
    return None


def read_request_with_scope(
    stream: socket.socket, diagnostics: bool = True
) -> tuple[str, bool, str | None]:
    buffered = b""
    while b"\r\n\r\n" not in buffered:
        chunk = stream.recv(READ_BYTES)
        if not chunk:
            return "", False, None
        buffered += chunk
        if len(buffered) > MAX_REQUEST_BYTES:
            raise RuntimeError("provider request exceeded its bound")
    head, _, rest = buffered.partition(b"\r\n\r\n")
    length = 0
    headers: dict[str, str] = {}
    for line in head.decode("latin-1").split("\r\n"):
        name, sep, value = line.partition(":")
        if sep:
            key = name.strip().lower()
            headers[key] = value.strip()
            if key == "content-length":
                length = int(value.strip() or 0)
    body = rest
    while len(body) < length:
        chunk = stream.recv(READ_BYTES)
        if not chunk:
            break
        body += chunk
        if len(body) > MAX_REQUEST_BYTES:
            raise RuntimeError("provider request body exceeded its bound")
    try:
        offered = json.loads(body.decode("utf-8")).get("tools") or []
        names = [t.get("name") or (t.get("function") or {}).get("name") for t in offered]
        if diagnostics:
            print("TOOLS_OFFERED " + json.dumps([n for n in names if n]), flush=True)
    except Exception:
        pass
    return prompt_of(body), has_tool_result(body), _extract_stream_scope(headers, body)


def validated_sse_delta_tokens(frame: str) -> int:
    """Count validated assistant text delta chunks from a parsed SSE frame.
    Rejects malformed JSON, raw substring spoofs, empty deltas, and prevents
    multiword frames from inflating token counts."""
    data_lines = [
        line[len("data:"):].strip()
        for line in frame.splitlines()
        if line.startswith("data:")
    ]
    if len(data_lines) != 1:
        return 0
    try:
        payload = json.loads(data_lines[0])
    except json.JSONDecodeError:
        return 0
    if not isinstance(payload, dict):
        return 0
    if payload.get("type") != "response.output_text.delta":
        return 0
    delta = payload.get("delta")
    if not isinstance(delta, str) or not delta.strip():
        return 0
    return 1


def _write_stream_metrics(path: Path, scopes: dict[str, int]) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps({"scopes": scopes}), encoding="utf-8")
    tmp.replace(path)


def serve(
    stream: socket.socket,
    delay_ms: int,
    served: list[float],
    stream_metrics_path: Path | None = None,
    stream_counter: dict[str, int] | None = None,
    stream_lock: threading.Lock | None = None,
) -> None:
    try:
        prompt, continuing, scope = read_request_with_scope(stream)
        # CHUNKED, with a terminating zero chunk. A stream that simply closed
        # after its last frame delivered every fragment and still ended the
        # execution as FAILED: the content decoded, and the response was then
        # truncated rather than finished. Framing each fragment is what makes
        # the end of the body a fact rather than a disconnection. Measured, not
        # guessed — the first run of this script recorded exactly that.
        stream.sendall(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
            b"Cache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n"
            b"Connection: close\r\n\r\n"
        )
        started = time.monotonic()
        for frame, delay in frames(delay_ms, prompt, continuing):
            if delay:
                time.sleep(delay)
            payload = frame.encode("utf-8")
            stream.sendall(b"%x\r\n" % len(payload) + payload + b"\r\n")
            if stream_metrics_path is not None and scope is not None and stream_counter is not None:
                tokens = validated_sse_delta_tokens(frame)
                if tokens > 0:
                    if stream_lock is not None:
                        with stream_lock:
                            stream_counter[scope] = stream_counter.get(scope, 0) + tokens
                            _write_stream_metrics(stream_metrics_path, dict(stream_counter))
                    else:
                        stream_counter[scope] = stream_counter.get(scope, 0) + tokens
                        _write_stream_metrics(stream_metrics_path, dict(stream_counter))
        stream.sendall(b"0\r\n\r\n")
        served.append(time.monotonic() - started)
    except (OSError, RuntimeError):
        # A client that walks away mid-stream is an ordinary outcome here — an
        # interrupted turn is one of the compared scenarios.
        pass
    finally:
        # CLOSE GRACEFULLY, and only the writing half. `shutdown(SHUT_RDWR)`
        # immediately after the terminating chunk tears the connection down
        # while the consumer may still be finishing with it, and the product
        # recorded the execution as FAILED even though every fragment had
        # arrived and the Step had settled `stop`. A half-close says "no more
        # body" without cancelling what the peer is still doing.
        #
        # OVER TLS A HALF-CLOSE IS NOT ENOUGH ON ITS OWN. A bare FIN with no
        # `close_notify` is an unclean shutdown, and rustls — the product's TLS
        # stack — can report that as an error rather than as an end of stream.
        # So send the alert first and half-close the plain socket it hands back.
        # `unwrap()` raises when the peer has already walked away, which is one
        # of the compared scenarios, so that failure is ordinary here.
        if isinstance(stream, ssl.SSLSocket):
            try:
                stream = stream.unwrap()
            except (ssl.SSLError, OSError):
                pass
        try:
            stream.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        try:
            stream.settimeout(2.0)
            while stream.recv(READ_BYTES):
                pass
        except OSError:
            pass
        stream.close()


def serve_tls(
    stream: socket.socket,
    context: ssl.SSLContext,
    delay_ms: int,
    served: list[float],
    stream_metrics_path: Path | None = None,
    stream_counter: dict[str, int] | None = None,
    stream_lock: threading.Lock | None = None,
) -> None:
    """Complete the handshake, then serve exactly as the plaintext path does.

    The TLS wrapper is the ONLY difference. Everything the product exercises
    downstream of it — the request parse, the scripted frames, the chunked
    framing, the half-close — is the same code, so a run over https is not a
    second implementation of the fixture that could drift from the first.
    """
    try:
        secured = context.wrap_socket(stream, server_side=True)
    except (ssl.SSLError, OSError) as error:
        # SAY SO, ON STDERR, BOUNDED. A silently dropped handshake makes "the
        # client does not trust this CA" indistinguishable from "nothing ever
        # connected", and those two have completely different causes and fixes.
        # Measured: a product whose platform verifier could not be given this CA
        # reported only "the provider connection failed before a response was
        # received", and the fixture end said nothing at all — so the run
        # carried no evidence of which end refused.
        print(f"handshake refused by peer: {type(error).__name__}: {error}",
              file=sys.stderr, flush=True)
        try:
            stream.close()
        except OSError:
            pass
        return
    serve(secured, delay_ms, served, stream_metrics_path, stream_counter, stream_lock)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--delay-ms", type=int, default=60)
    parser.add_argument("--repeat-call-ids", action="store_true",
                        help="mint the pre-correction flat id (call_j04_<tool>) for every call, "
                             "reproducing the delivered corpus in which two dispatches share one "
                             "id. This is the negative case for call-id correlation; keep it "
                             "reproducible rather than only historical.")
    parser.add_argument("--receipt", type=Path, required=True)
    # TLS IS OPT-IN, so every existing caller keeps the exact behaviour it has.
    # The in-tree callers reach this fixture through the product's testkit seam,
    # which accepts `http://127.0.0.1:<port>/` and nothing else; turning TLS on
    # by default would break all of them to serve one new caller.
    parser.add_argument("--tls-cert", type=Path,
                        help="leaf certificate; enables https (requires --tls-key)")
    parser.add_argument("--tls-key", type=Path,
                        help="leaf private key; enables https (requires --tls-cert)")
    arguments = parser.parse_args()
    global _repeat_call_ids
    _repeat_call_ids = arguments.repeat_call_ids

    if bool(arguments.tls_cert) != bool(arguments.tls_key):
        # REFUSE THE HALF-CONFIGURED CASE. Serving plaintext because only one of
        # the two arrived would hand a caller who ASKED for https a plaintext
        # endpoint, and the product would then refuse it with a scheme error
        # naming nothing about the real mistake.
        raise SystemExit("REFUSED: --tls-cert and --tls-key must be given together")

    context: ssl.SSLContext | None = None
    if arguments.tls_cert is not None:
        for path in (arguments.tls_cert, arguments.tls_key):
            if not path.is_file():
                raise SystemExit(f"REFUSED: TLS material {path} does not exist")
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        # The product's client is rustls with TLS 1.2/1.3 only. Pinning the
        # floor here means a handshake failure is a certificate problem rather
        # than an ambiguity about which of the two ends dropped the version.
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(str(arguments.tls_cert), str(arguments.tls_key))

    scheme = "https" if context is not None else "http"
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", arguments.port))
    listener.listen(64)
    address = listener.getsockname()
    served: list[float] = []
    stream_metrics_path = arguments.receipt.with_suffix(".stream-tokens.json")
    stream_counter: dict[str, int] = {}
    stream_lock = threading.Lock()
    _write_stream_metrics(stream_metrics_path, {})
    arguments.receipt.write_text(
        json.dumps(
            {
                "contract": "native-tui-loopback-provider/v1",
                "endpoint": f"{scheme}://{address[0]}:{address[1]}",
                "stream_metrics": str(stream_metrics_path),
                "tls": None if context is None else {
                    "cert": str(arguments.tls_cert),
                    "minimum_version": "TLSv1_2",
                    "why": (
                        "the packaged product refuses a non-https provider base at "
                        "every precedence level; the fixture meets that standard "
                        "rather than the guard being weakened to meet the fixture"
                    ),
                },
                "delay_ms": arguments.delay_ms,
                "fragments_after_echo": len(TEXT.split(" ")),
                "max_echo_chars": MAX_ECHO_CHARS,
                "note": (
                    "Scripted provider on loopback. The transport, streaming decode, "
                    "durable capture and both clients are the real ones; only the model "
                    "is replaced, so a comparative run costs no model spend."
                ),
            },
            indent=2,
        ),
        encoding="utf-8",
    )
    print(f"{scheme}://{address[0]}:{address[1]}", flush=True)
    try:
        while True:
            stream, _ = listener.accept()
            if context is not None:
                # HANDSHAKE IN THE WORKER, NOT HERE. Wrapping on the accept
                # thread makes one client that opens a connection and never
                # completes the handshake block every later connection, and the
                # product opens more than one. A failed handshake is also an
                # ordinary event — a probe, or a client that does not trust this
                # CA — and it must close that connection, not end the fixture.
                threading.Thread(
                    target=serve_tls,
                    args=(
                        stream,
                        context,
                        arguments.delay_ms,
                        served,
                        stream_metrics_path,
                        stream_counter,
                        stream_lock,
                    ),
                    daemon=True,
                ).start()
                continue
            threading.Thread(
                target=serve,
                args=(
                    stream,
                    arguments.delay_ms,
                    served,
                    stream_metrics_path,
                    stream_counter,
                    stream_lock,
                ),
                daemon=True,
            ).start()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
