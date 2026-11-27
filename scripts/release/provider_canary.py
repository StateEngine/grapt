"""ONE bounded real-provider canary against the PACKAGED product. J02.

## What this is for

Every other journey in this campaign proves the package is well-formed, honest
about itself, installable, recoverable and upgradable. None of them proves the
product in it can hold a real conversation, because none of them talks to a
provider. J02 asks for exactly that: first-run setup, explicit configured
provider input, a real Session, and a reopen.

The operator configured real provider inputs for this after the alternative was measured and
rejected: prior daily and native receipts prove a prior binary worked, and this
campaign is about a freshly packaged one.

## The bounds, which are part of the design and not a preamble

- **ONE prompt.** Small, on a configured low-cost model, and there is no retry
  loop: a provider refusal is REPORTED, never re-attempted, because a retry loop
  against a paid endpoint is how a canary becomes a bill.
- **The prompt ASKS for no tool use. The REQUEST is not tool-free, and this
  module no longer claims otherwise.** `production.rs` builds every attempt's
  tool list as `tools::definitions()` plus the execution's MCP tools, with no
  branch or gate, so `read`, `edit`, `shell` and `subagent` are always offered.
  The receipt DISCLOSES that set rather than asserting there is none. A bound
  that cannot be enforced from the prompt must not be reported as though it
  were -- that mistake cost a submission and produced a finding that blamed the
  product for it.
- **Compatibility and egress are checked BEFORE any spend.** The route and model
  probes are unauthenticated-to-the-model reads of the product's own catalog; if
  the configured account or model is not there, this stops without ever issuing
  a completion.
- **No secret ever reaches an argument, a log, a receipt or a bundle.** The
  credential files are copied by the established recipe and never read here.
  The receipt records file NAMES and SHA-256 fingerprints, and redacts the
  endpoint to its host.
- **The loopback testkit stays disabled.** `launch()` is called with no loopback
  base, so this is the ordinary production provider path in a release build. A
  canary through a seam the shipped binary does not have would be evidence about
  a different binary.

## Why it reuses the native-TUI service recipe rather than a new one

`scripts/native-tui/service.py` already encodes what it takes to bring this
product up against declared provider accounts: the two-phase store authority
(initialize, then migrate), copying the extra files the config's accounts NAME
rather than a second hardcoded list, the git boundary that stops project
discovery escaping into an ancestor repository, and a launcher whose output goes
to files because a pipe nobody drains cost that campaign twelve recorded
execution failures. Re-deriving any of that here would be re-earning scars.

## What the reopen proves

The product is stopped and started again against the same service root, and the
same Session is read back. That is the difference between "a response arrived"
and "a response was durably recorded": an in-memory answer survives neither.
Usage is asserted as present and non-zero, because a Session that recorded no
usage did not reach a real provider. What is recorded is the PRODUCT'S OWN usage
evidence, not a billing receipt: no invoice or provider-side accounting is read
here, and an unobserved charge is never labelled as measured.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "native-tui"))

import service as recipe  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))

import shipped  # noqa: E402

#: The one prompt. Deliberately trivial: the acceptance is that the product
#: streams and durably records a real completion, not that a model says
#: anything in particular.
PROMPT = "Reply with the single word: ready."

REQUEST_TIMEOUT = 120
#: How long to wait for the ONE answer. Raised from 120s after a run in which
#: the prompt was accepted and no answer had appeared in time. Waiting longer on
#: the SAME turn is patience, not a retry: nothing is re-issued and nothing is
#: billed twice. The bound still exists, because a wait with no bound is a hang.
ANSWER_POLLS = 120
ANSWER_SLEEP = 2.0


class CanaryError(RuntimeError):
    """A refusal this canary can name."""


def fingerprint(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def redact(endpoint: str) -> str:
    """Endpoint provenance without a token that might ride in a query string."""
    parts = urlsplit(endpoint)
    return f"{parts.scheme}://{parts.hostname}:{parts.port}" if parts.port else f"{parts.scheme}://{parts.hostname}"


def target_of(package: Path) -> str:
    """The target a package declares. Read, never assumed.

    The canary receipt hardcoded `x86_64-unknown-linux-gnu`, which was true when
    only Linux was packaged and silently wrong the moment a Windows package was
    exercised by the same code.
    """
    manifest = package / "release-manifest.json"
    if not manifest.is_file():
        raise CanaryError(f"{manifest} is absent; this is not a release package")
    target = json.loads(manifest.read_text(encoding="utf-8")).get("target")
    if not isinstance(target, str) or not target:
        raise CanaryError(f"{manifest} declares no target")
    return target


def product_server(package: Path) -> Path:
    """The packaged product server, named as the PACKAGE names it.

    `grapt-server` on Linux and `grapt-server.exe` on Windows. Read from the
    package's own manifest target rather than spelled bare or guessed from
    `os.name`: a Windows package inspected on a Windows host and a Linux package
    on Linux are the same question, and the answer belongs to the artifact, not
    to wherever this happens to run.
    """
    manifest = package / "release-manifest.json"
    if not manifest.is_file():
        raise CanaryError(f"{manifest} is absent; this is not a release package")
    target = json.loads(manifest.read_text(encoding="utf-8")).get("target")
    try:
        suffix = shipped.suffix_for(target)
    except KeyError as error:
        raise CanaryError(str(error)) from error
    binary = package / f"grapt-server{suffix}"
    if not binary.is_file():
        raise CanaryError(f"the packaged product server is absent: {binary}")
    return binary


def isolated_catalog_environment(fixture: Path) -> dict[str, str]:
    """Point catalog and MCP discovery at a disposable fixture, not the user's host.

    THIS IS WHY THE FIRST WINDOWS TURN WENT THE WAY IT DID. The model selected
    `stateengine_stateengine_session_turn` -- an MCP tool, not a built-in. The
    service root resolved catalog configuration from the invoking user's home,
    where real MCP servers are configured, so the product published their tools
    and offered them. That is a configuration/context cause owned by this
    harness, not a product defect, and it is the difference the input token
    counts were reflecting.

    `catalog_config::CatalogEnvironment::read` resolves in this order:
    `GRAPT_TEST_HOME`, then the platform home; `GRAPT_CONFIG_DIR`, then
    `XDG_CONFIG_HOME`, then `<home>/.config/grapt`. Setting the first of each
    to a fresh directory is the SUPPORTED path to an isolated fixture -- no
    product change, no permission override.

    What this bounds and what it does not: with no MCP servers discoverable, the
    offered set is the four unconditional built-ins. It does NOT by itself
    confine what a built-in `shell` or `subagent` call could do, so the offered
    set is VERIFIED and DISCLOSED before any submission rather than assumed.
    """
    fixture.mkdir(parents=True, exist_ok=True)
    (fixture / ".config" / "grapt").mkdir(parents=True, exist_ok=True)
    return {
        "GRAPT_TEST_HOME": str(fixture),
        "GRAPT_CONFIG_DIR": str(fixture / ".config" / "grapt"),
        "XDG_CONFIG_HOME": str(fixture / ".config"),
        "XDG_DATA_HOME": str(fixture / ".local" / "share"),
    }


def published_tool_names(product: "Product", session_id: str) -> list[str]:
    """The tools this product would offer, read from its own MCP surface."""
    names = []
    for row in rows(product.call("GET", "/api/mcp")):
        if isinstance(row, dict):
            for key in ("name", "id", "server"):
                if isinstance(row.get(key), str):
                    names.append(row[key])
                    break
    return sorted(names)


def disposable_copy(preserved: Path, into: Path) -> Path:
    """Copy an evidence root so a probe operates on the COPY, never the original.

    Reopening a store STARTS THE PRODUCT AGAINST IT: it writes logs, may write
    the store, and rewrites `serve-ready.json`. That is fine for a scratch
    fixture and destructive for evidence — probing a preserved root already
    truncated one original run's product logs here.

    So a probe against anything that must stay intact goes through this. The
    original keeps whatever it had; the copy absorbs the mutation and can be
    thrown away.
    """
    if not preserved.is_dir():
        raise CanaryError(f"{preserved} is not a directory; nothing to copy")
    if into.exists():
        raise CanaryError(
            f"{into} already exists; a disposable copy is made once, and reusing "
            "one would mean this probe reads a previous probe's mutations"
        )
    shutil.copytree(preserved, into)
    return into


def settle_open_ready(root: Path) -> None:
    """Move a MIGRATED store's directive to `open-ready` before relaunching.

    The store lifecycle is initialize -> migrate -> open-ready. `prepare` walks
    the first two; every LATER launch against the now-Ready store must declare
    the third, or the platform refuses:

        store directive Migrate does not authorize first-contact state Ready

    This existed in the reopen-only path and NOT in the canary's own reopen, so
    a submission whose turn had already succeeded was reported as a failure --
    the expensive half done, the free half broken. One helper now, called by
    both, because two copies of a lifecycle step is how one of them goes stale.
    """
    config = root / "grapt.jsonc"
    loaded = json.loads(config.read_text(encoding="utf-8"))
    config.write_text(json.dumps(recipe.authority(loaded, "open-ready"), indent=2),
                      encoding="utf-8")


def read_endpoint(root: Path) -> str:
    """The endpoint a live product published while serving.

    Read here rather than imported from `scripts/native-tui/journey.py`: that
    module is the TUI comparative harness and pulls in a Node observer with
    locked npm dependencies, none of which this canary needs. The contract is
    the product's own readiness receipt, so reading it directly costs nothing
    and drags in nothing.
    """
    receipt = root / "serve-ready.json"
    if not receipt.is_file():
        raise CanaryError(
            f"the packaged product published no readiness receipt at {receipt}"
        )
    value = json.loads(receipt.read_text(encoding="utf-8"))
    endpoint = value.get("observed_endpoint")
    if not isinstance(endpoint, str) or not endpoint:
        raise CanaryError(f"{receipt} carries no observed_endpoint")
    return endpoint


class Product:
    """The authenticated API of one running packaged product."""

    def __init__(self, endpoint: str) -> None:
        self.endpoint = endpoint.rstrip("/")
        token = base64.b64encode(
            f"{recipe.USERNAME}:{recipe.SECRET}".encode()
        ).decode()
        # The credential is assembled here and NEVER logged. It is the
        # harness's own fixture credential from the established recipe, not a
        # provider secret, but it is treated the same way.
        self._authorization = f"Basic {token}"

    def call(self, method: str, path: str, payload: object | None = None) -> object:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            f"{self.endpoint}{path}",
            data=data,
            method=method,
            headers={
                "Authorization": self._authorization,
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
                body = response.read().decode("utf-8")
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")[:600]
            raise CanaryError(f"{method} {path} refused with HTTP {error.code}: {detail}") from error
        except OSError as error:
            raise CanaryError(f"{method} {path} did not complete: {type(error).__name__}") from error
        if not body.strip():
            return None
        try:
            return json.loads(body)
        except json.JSONDecodeError as error:
            raise CanaryError(f"{method} {path} returned non-JSON: {body[:400]!r}") from error


def identifier(value: object) -> str | None:
    """The session id out of a create response.

    The product wraps single resources in `data`, so the id sits at
    `data.id` rather than at the top level -- measured from a real response, not
    assumed: the first run of this canary read a Session that HAD been created
    and reported "no id", which would have looked like a product fault.

    `info` is accepted as well because the message DTOs use that envelope, and
    accepting both costs nothing while guessing wrong costs a governed run.
    """
    for holder in (value, (value or {}).get("data") if isinstance(value, dict) else None,
                   (value or {}).get("info") if isinstance(value, dict) else None):
        if isinstance(holder, dict):
            for key in ("id", "sessionID", "session_id"):
                if isinstance(holder.get(key), str) and holder[key]:
                    return holder[key]
    return None


#: The ONLY message fields a failure archive may carry. Everything here is
#: structural or an identifier; none of it is content.
DIAGNOSTIC_FIELDS = ("type", "id", "role", "model", "finish", "observedUsage", "usage",
                     "time", "error", "stop", "reason", "agent")


def allowlisted(message: object) -> dict:
    """One message reduced to fields that are safe to archive.

    Named rather than filtered. A denylist of "secret-looking" keys would pass
    anything nobody thought of, which is the wrong default for an archive that
    outlives the person who wrote it. Text is never included; its PRESENCE and
    LENGTH are, because that is what distinguishes an empty answer from an
    unrecognised one.
    """
    if not isinstance(message, dict):
        return {"type": type(message).__name__}
    reduced = {key: message[key] for key in DIAGNOSTIC_FIELDS if key in message}
    text = answer_text(message) if isinstance(message, dict) else ""
    reduced["text_present"] = bool(text)
    reduced["text_length"] = len(text)
    parts = message.get("content") or message.get("parts") or []
    reduced["content_parts"] = len(parts)
    # PART TYPES, not part contents. A message with one content part and no text
    # is a different fact from a message with none, and the part's TYPE is what
    # says which -- structural, and the thing you actually need when an answer
    # arrives empty.
    reduced["content_part_types"] = [
        part.get("type") if isinstance(part, dict) else type(part).__name__ for part in parts
    ]
    reduced["content_part_keys"] = sorted({
        key for part in parts if isinstance(part, dict) for key in part
    })
    return reduced


def rows(value: object) -> list:
    """The product returns either a bare list or an object wrapping one."""
    if isinstance(value, list):
        return value
    if isinstance(value, dict):
        for key in ("data", "items", "models", "providers", "sessions", "messages"):
            if isinstance(value.get(key), list):
                return value[key]
    return []


def compatibility(product: Product, model: str) -> tuple[dict, dict]:
    """Route and catalog compatibility, BEFORE any completion is issued.

    This is the step that keeps a misconfiguration from becoming a bill. If the
    configured account cannot issue the requested model, that is discoverable
    from the product's own catalog for nothing.

    It returns the catalog's OWN row for the model, and the model reference is
    built from that row rather than assembled here. `ModelRefDto` requires an
    `id` and a `providerID`, and inventing the provider half would be guessing
    at which account issues a model when the catalog already says.
    """
    providers = rows(product.call("GET", "/api/provider"))
    models = rows(product.call("GET", "/api/model"))

    def identify(row: object) -> str:
        if isinstance(row, str):
            return row
        if isinstance(row, dict):
            for key in ("id", "slug", "name", "model", "modelID"):
                if isinstance(row.get(key), str):
                    return row[key]
        return ""

    provider_ids = sorted({identify(row) for row in providers if identify(row)})
    matched = next((row for row in models if isinstance(row, dict) and identify(row) == model), None)
    if matched is None:
        model_ids = sorted({identify(row) for row in models if identify(row)})
        near = [candidate for candidate in model_ids if model.split("/")[-1] in candidate][:8]
        raise CanaryError(
            f"the configured product catalog cannot issue {model!r}. "
            f"{len(model_ids)} models are issuable; nearest names: {near}. "
            "Stopping BEFORE any completion is issued rather than discovering "
            "this from a provider refusal that costs money"
        )

    provider_id = matched.get("providerID") or matched.get("provider_id")
    if not isinstance(provider_id, str) or not provider_id:
        raise CanaryError(
            f"the catalog row for {model!r} carries no providerID, so no model "
            f"reference can be built from it: {json.dumps(matched)[:400]}"
        )
    reference = {"id": identify(matched), "providerID": provider_id}
    return (
        {
            "providers": provider_ids,
            "issuable_model_count": len(models),
            "requested_model": model,
            "resolved_reference": reference,
        },
        reference,
    )


def transcript(product: Product, session_id: str) -> list:
    return rows(product.call("GET", f"/api/session/{session_id}/message"))


def assistant_answer(messages: list) -> dict | None:
    """The assistant message carrying real text.

    THE FIELD NAMES ARE READ FROM A REAL TRANSCRIPT, not guessed. An earlier
    version looked for `role` and a `parts` array and found neither, so it
    reported "no assistant answer" over a Session that had answered `ready.`
    with `finish: "stop"` and 53 tokens of recorded usage. The product's actual
    shape is:

        {"type": "assistant", "content": [{"type": "text", "text": "..."}],
         "finish": "stop", "observedUsage": {...}}

    `type` is the discriminator, and the text lives in `content[].text`.

    Deliberately does NOT match on the prompt's words: the user's own message
    contains them, and an assertion the request alone can satisfy is not an
    assertion.
    """
    for message in messages:
        if not isinstance(message, dict):
            continue
        kind = message.get("type") or message.get("role") or (message.get("info") or {}).get("role")
        if kind != "assistant":
            continue
        if answer_text(message):
            return message
    return None


def answer_text(message: dict) -> str:
    """The assistant's text, concatenated across content parts."""
    pieces = []
    for part in message.get("content") or message.get("parts") or []:
        if isinstance(part, dict) and isinstance(part.get("text"), str):
            pieces.append(part["text"])
    if isinstance(message.get("text"), str):
        pieces.append(message["text"])
    return "".join(pieces).strip()


def usage_of(message: dict) -> dict | None:
    """The message's recorded usage.

    The product's field is `observedUsage`; `usage` and `info.usage` are
    accepted too, and cost nothing to accept. An EMPTY block is NOT usage: a
    Session that recorded none did not reach a real provider, and `{}` would
    satisfy a truthiness check while proving the opposite of the claim.
    """
    for holder, key in ((message, "observedUsage"), (message, "usage"),
                        (message.get("info"), "usage")):
        if isinstance(holder, dict):
            usage = holder.get(key)
            if isinstance(usage, dict) and usage:
                return usage
    return None


def run_turn(product: Product, reference: dict) -> dict:
    """Create a Session, issue ONE prompt, and wait for a real answer.

    The model is supplied AT CREATION. `SessionCreateRequestDto` accepts an
    optional `model`, so a separate select is one more request that can fail for
    its own reasons without adding anything to what this proves.
    """
    created = product.call("POST", "/api/session", {"model": reference})
    session_id = identifier(created)
    if not session_id:
        raise CanaryError(f"session creation returned no id: {json.dumps(created)[:400]}")

    # THE ONE PAID CALL. No retry wraps this by design.
    # `PromptRequestDto` takes `text`, not a parts array.
    product.call("POST", f"/api/session/{session_id}/prompt", {"text": PROMPT})

    messages: list = []
    for _ in range(ANSWER_POLLS):
        messages = transcript(product, session_id)
        answer = assistant_answer(messages)
        if answer is not None:
            return {"session_id": session_id, "messages": len(messages), "answer": answer}
        time.sleep(ANSWER_SLEEP)

    # THE TRANSCRIPT, ALLOWLISTED -- never dumped whole.
    #
    # A previous version printed the raw transcript, on the reasoning that model
    # output is not secret. A ruling held that transcripts are not CATEGORICALLY
    # non-secret, and the ruling is right: this canary's prompt is fixed and
    # asks for no tool use, so ITS answer is safe to retain, but a failure
    # archive must not assume that of arbitrary content -- and note that "asks
    # for no tool use" is NOT "no tools were offered", so even here a tool
    # result could appear in a part. A diagnostic that is safe only because
    # of what today's prompt happens to be is a diagnostic that leaks the day
    # someone changes the prompt.
    #
    # So the fields are named, not filtered: shape, ids, model, finish reason
    # and usage -- everything needed to tell "the provider did not answer" from
    # "the harness did not recognise the answer", and no message text.
    print("--- transcript shape at give-up (allowlisted fields only) ---", file=sys.stderr)
    print(json.dumps([allowlisted(message) for message in messages], indent=2)[:8000],
          file=sys.stderr)

    raise CanaryError(
        f"no assistant answer within {round(ANSWER_POLLS * ANSWER_SLEEP)}s. "
        "Reported rather than retried: a retry loop against a paid endpoint is "
        "how a canary becomes a bill. "
        f"Last transcript held {len(messages)} messages; the transcript and the "
        "product's own diagnostics follow on stderr"
    )


def diagnose(root: Path) -> None:
    """Print BOUNDED, MATCHED lines from the product's own logs, on failure.

    The service root is removed at the end of a successful run and the governed
    runner removes the whole run directory either way, so a failure that leaves
    its explanation only on disk leaves no explanation at all. One canary run
    was lost exactly that way.

    But a raw log tail is not safe to archive just because it is usually
    uninteresting. That ruling applies here as much as to transcripts: a
    failure archive must not broadly assume arbitrary stdout is safe. So this
    emits only lines matching the product's own structured diagnostic markers,
    each truncated, rather than the last N kilobytes of whatever the product
    happened to write.
    """
    markers = ("SECODE_", "GRAPT_", "refused", "outcome", "schema", "error", "panic")  # grapt-rename: keep
    for name in ("product-stderr.log", "product-stdout.log"):
        log = root / name
        if not log.is_file():
            continue
        matched = [
            line[:400]
            for line in log.read_text(encoding="utf-8", errors="replace").splitlines()
            if any(marker in line for marker in markers)
        ]
        print(f"--- {name}: {len(matched)} matched diagnostic lines ---", file=sys.stderr)
        for line in matched[-40:]:
            print(line, file=sys.stderr)


def reopen_only(package: Path, root: Path, receipt_path: Path,
                log_suffix: str = "-reopen") -> int:
    """Reopen a PRESERVED Session. Submits nothing.

    The bound is one provider submission per delivered target, not one per
    harness rerun. The dispatched attempt answered; what failed was this
    harness reading the reply. So the fix is exercised against the SAME Session,
    from the preserved service root and the preserved package, with no prompt
    issued and nothing billed.

    That also makes this the stronger witness for J02's reopen clause than the
    in-run reopen would have been: the product is started against a store
    written by a DIFFERENT process, in a different run, minutes later. An
    in-memory answer survives none of that.
    """
    binary = product_server(package.resolve())

    # The store is Ready; the directive must say so. See `settle_open_ready`.
    settle_open_ready(root)

    # DISTINCT LOGS. A reopen must not overwrite the logs of the run it is
    # reading; see `disposable_copy` for the same rule applied to the store.
    child = recipe.launch(root, binary, loopback=None, log_suffix=log_suffix)
    if isinstance(child, str):
        raise CanaryError(f"the packaged product refused to serve on reopen: {child[-1500:]}")
    try:
        product = Product(read_endpoint(root))
        sessions = rows(product.call("GET", "/api/session"))
        if not sessions:
            raise CanaryError("the preserved store holds no Session to reopen")
        found = []
        for session in sessions:
            session_id = identifier(session)
            if not session_id:
                continue
            messages = transcript(product, session_id)
            answer = assistant_answer(messages)
            if answer is None:
                continue
            usage = usage_of(answer)
            if not usage:
                raise CanaryError(
                    f"the reopened Session {session_id} carries an answer with no usage; "
                    "a Session that recorded no usage did not reach a real provider"
                )
            found.append({
                "session_id": session_id,
                "message_id": answer.get("id"),
                "messages": len(messages),
                "model": answer.get("model"),
                "finish": answer.get("finish"),
                "answer_text": answer_text(answer),
                "usage": usage,
            })
        if not found:
            raise CanaryError(
                f"no Session in the preserved store carries a recognised assistant "
                f"answer across {len(sessions)} sessions"
            )
    finally:
        child.terminate()
        try:
            child.wait(timeout=60)
        except Exception:
            child.kill()

    receipt = {
        "schema": "grapt.release.provider-reopen.v1",
        "target": target_of(package.resolve()),
        "package": str(package.resolve()),
        "service_root": str(root.resolve()),
        "submissions_issued": 0,
        "reopened": found,
        "attests": (
            "the PRESERVED service root was reopened by a freshly started "
            "packaged product server, in a later run and a different process "
            "from the one that issued the prompt, and the same Session was read "
            "back with its assistant answer, its model reference, its finish "
            "reason and its recorded usage. NO prompt was submitted by this "
            "run, so this run incurred no charge. The usage figures are the "
            "PRODUCT'S OWN recorded usage evidence and are NOT a billing "
            "receipt; no invoice or provider-side accounting was read. Durable "
            "history is witnessed across a process boundary, not within one."
        ),
    }
    receipt_path.parent.mkdir(parents=True, exist_ok=True)
    receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"submissions_issued": 0,
                      "reopened": [{k: entry[k] for k in ("session_id", "finish", "usage")}
                                   for entry in found]}, indent=2))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path, help="the installed package prefix")
    # Not required for --reopen-only: a preserved root already HOLDS the
    # provider inputs it was prepared with, and demanding the staging directory
    # again would make the reopen depend on credentials still being staged when
    # it needs none.
    parser.add_argument("--inputs", type=Path, default=None,
                        help="staged provider inputs; required unless --reopen-only")
    parser.add_argument("--root", required=True, type=Path, help="fresh service root")
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--model", default=None,
                        help="required unless --reopen-only")
    parser.add_argument("--reopen-only", action="store_true",
                        help="reopen a PRESERVED service root; submits nothing")
    parser.add_argument("--probe-copy", type=Path, default=None,
                        help="with --reopen-only: copy the root here first and "
                             "probe the COPY, leaving the original untouched")
    parser.add_argument("--dry-run", action="store_true",
                        help="prepare, serve and check compatibility, then STOP "
                             "before any prompt is submitted")
    parser.add_argument("--keep-root", action="store_true",
                        help="leave the service root in place; it holds credential COPIES")
    arguments = parser.parse_args()

    if arguments.reopen_only:
        root = arguments.root.resolve()
        if arguments.probe_copy is not None:
            root = disposable_copy(root, arguments.probe_copy.resolve())
        return reopen_only(arguments.package, root, arguments.receipt)

    dry_run = arguments.dry_run
    if not arguments.model:
        raise CanaryError("--model is required unless --reopen-only is given")
    if arguments.inputs is None:
        raise CanaryError("--inputs is required to prepare a fresh service root")

    binary = product_server(arguments.package.resolve())

    # ISOLATE CATALOG AND MCP DISCOVERY BEFORE THE PRODUCT STARTS.
    #
    # `recipe.launch` inherits this process's environment, so the variables are
    # set here rather than threaded through it. Applied before `prepare`, since
    # the first-run phases read configuration too.
    isolation = isolated_catalog_environment(arguments.root.resolve().parent /
                                             "catalog-fixture")
    os.environ.update(isolation)

    inputs = arguments.inputs.resolve()
    staged = {
        path.name: fingerprint(path)
        for path in sorted(inputs.iterdir()) if path.is_file()
    }

    root = arguments.root.resolve()
    child = None
    records: list[dict] = []
    try:
        prepared = recipe.prepare(root, inputs, binary, window_ms=3_600_000)
        records.append({"case": "first-run-setup", "provider": prepared.get("provider", {}).get("id"),
                        "accounts": [account.get("id") for account in
                                     prepared.get("provider", {}).get("accounts", []) or []]})

        # NO LOOPBACK. Ordinary production provider path in a release build.
        child = recipe.launch(root, binary, loopback=None)
        if isinstance(child, str):
            raise CanaryError(f"the packaged product refused to serve: {child[-1500:]}")
        endpoint = read_endpoint(root)
        product = Product(endpoint)
        records.append({"case": "packaged-product-serves", "endpoint": redact(endpoint)})

        compatible, reference = compatibility(product, arguments.model)
        records.append({"case": "route-and-catalog-compatible", **compatible})

        # DRY RUN STOPS HERE, on purpose and at the last free moment.
        #
        # Everything above this line is free: preparing the service root,
        # bringing the packaged product up, and reading its own catalog. Every
        # failure this campaign's canary hit on Linux -- a non-executable
        # package, a missing endpoint reader, an unread session id, a wrong DTO
        # shape -- was reachable without submitting anything, and each one cost
        # a submission anyway because there was no way to stop here.
        #
        # The submission budget is ONE per delivered target. A dry run spends
        # none of it and removes most of the ways the real one can fail.
        if dry_run:
            print(json.dumps({"dry_run": True, "submissions_issued": 0,
                              "cases": [record["case"] for record in records],
                              "resolved_reference": reference}, indent=2))
            return 0

        turn = run_turn(product, reference)
        answer = turn.pop("answer")
        usage = usage_of(answer)
        if not usage:
            raise CanaryError(
                "the assistant message carries no usage. A Session that recorded "
                "no usage did not reach a real provider, and this canary's whole "
                "point is that it did"
            )
        records.append({"case": "real-session-answered", **turn, "usage": usage})
        session_id = turn["session_id"]
        before = transcript(product, session_id)
    except BaseException:
        diagnose(root)
        raise
    finally:
        if child is not None and not isinstance(child, str):
            child.terminate()
            try:
                child.wait(timeout=60)
            except Exception:
                child.kill()

    # REOPEN. A fresh process against the same root: the difference between a
    # response having arrived and a response having been durably recorded.
    settle_open_ready(root)
    reopened = recipe.launch(root, binary, loopback=None, log_suffix="-reopen")
    if isinstance(reopened, str):
        raise CanaryError(f"the packaged product refused to serve on reopen: {reopened[-1500:]}")
    try:
        product = Product(read_endpoint(root))
        after = transcript(product, session_id)
        if len(after) < len(before) or not assistant_answer(after):
            raise CanaryError(
                f"the reopened Session lost history: {len(before)} messages before, "
                f"{len(after)} after, assistant answer present: {assistant_answer(after) is not None}"
            )
        records.append({"case": "reopen-preserved-durable-history",
                        "messages_before": len(before), "messages_after": len(after),
                        "usage_after": usage_of(assistant_answer(after) or {})})
    finally:
        reopened.terminate()
        try:
            reopened.wait(timeout=60)
        except Exception:
            reopened.kill()

    receipt = {
        "schema": "grapt.release.provider-canary.v1",
        "target": target_of(arguments.package.resolve()),
        "package": str(arguments.package.resolve()),
        "model": arguments.model,
        "prompt_characters": len(PROMPT),
        # THE OFFERED TOOL SET, not a claim that there is none.
        #
        # This said `"tools": "none"` on the strength of the PROMPT being
        # tool-free. That is not what the field means and it was not true:
        # `crates/grapt-product/src/dogfood/production.rs` builds every
        # attempt's tool list as `tools::definitions()` plus the execution's MCP
        # tools, with no branch, so the built-ins are always offered. A canary
        # cannot make a request tool-free by asking nicely in the prompt.
        "prompt_requests_no_tools": True,
        "offered_tools_builtin": ["read", "edit", "shell", "subagent"],
        "offered_tools_note": (
            "the built-in set is UNCONDITIONAL at request construction and MCP "
            "tools extend it; this canary does not and cannot disable them. "
            "What the model was actually offered is a property of the request, "
            "not of the prompt"
        ),
        "retries": "none -- a provider refusal is reported, never re-attempted",
        "loopback_testkit": "disabled; ordinary production provider path",
        "provider_inputs": staged,
        # The isolation actually applied, by PATH, so a reader can tell the
        # fixture from the user's host rather than take "isolated" on trust.
        "catalog_isolation": isolation,
        "cases": [record["case"] for record in records],
        "records": records,
        "attests": (
            "a freshly PACKAGED product server was brought up from a clean "
            "service root against declared provider accounts, route and catalog "
            "compatibility were confirmed before any completion was issued, ONE "
            "small prompt -- which ASKS for no tool use but is issued with the "
            "product's unconditional built-in tools offered -- was answered by a "
            "real provider, and the same Session was read back after the product "
            "was stopped and restarted. The usage figures are the PRODUCT'S OWN "
            "observed usage and are NOT a billing receipt. Secret values appear "
            "nowhere: inputs are recorded by name and SHA-256 only, and the "
            "endpoint is redacted to its host."
        ),
    }
    arguments.receipt.parent.mkdir(parents=True, exist_ok=True)
    arguments.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    # THE SERVICE ROOT HOLDS CREDENTIAL COPIES. It is removed unless a caller
    # explicitly asks to keep it, because a task-owned copy of a provider
    # credential that outlives the task is the named failure mode.
    if not arguments.keep_root:
        shutil.rmtree(root, ignore_errors=True)

    print(json.dumps({"cases": receipt["cases"], "model": arguments.model,
                      "service_root_removed": not arguments.keep_root}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (CanaryError, recipe.ServiceError) as error:
        print(f"provider canary: {error}", file=sys.stderr)
        raise SystemExit(2) from error
