#!/usr/bin/env python3
"""Provision this campaign's isolated OpenRouter account input.

DAILY-USE-BACKEND-01 J01. The credential is read from the environment and
written to one file inside the isolated service root. It is never printed,
logged, echoed into the discovery snapshot or the provenance receipt, and never
returned to a caller. The snapshot this writes carries only non-secret model
rows plus the provenance needed to reproduce it.

Live account discovery, not a hand-authored list: the account's own model set
is read from the OpenRouter API with the configured key, then intersected with
the public catalog the product already ships. The intersection is the honest
answer to "which models can this product actually issue for this account" —
a model the account offers but the catalog cannot describe has no metadata to
show a user, and a catalog model the account does not offer is not issuable.
Both exclusions are counted in the receipt rather than silently dropped.
"""

import argparse
import datetime
import json
import os
import sys
import urllib.error
import urllib.request

MODELS_URL = "https://openrouter.ai/api/v1/models"
KEY_URL = "https://openrouter.ai/api/v1/key"
TIMEOUT_SECONDS = 60
PROVIDER = "openrouter"


def refuse(detail):
    print(f"OPENROUTER PROVISION REFUSED: {detail}", file=sys.stderr)
    raise SystemExit(2)


def authorized_get(url, key):
    request = urllib.request.Request(url, headers={"Authorization": f"Bearer {key}"})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            return response.status, json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        # The body can carry an auth or billing refusal; it is reported as the
        # provider stated it, never replaced with a guess.
        body = error.read().decode("utf-8", "replace")[:400]
        return error.code, {"provider_refusal": body}
    except OSError as error:
        refuse(f"the discovery request did not complete: {type(error).__name__}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--service-root", required=True)
    parser.add_argument("--catalog", required=True)
    parser.add_argument("--discovery-name", default="openrouter-account-models.json")
    parser.add_argument("--credential-name", default="openrouter.credential")
    arguments = parser.parse_args()

    key = os.environ.get("OPENROUTER_API_KEY")
    if not key:
        refuse("OPENROUTER_API_KEY is absent from this process environment")

    status, key_info = authorized_get(KEY_URL, key)
    if status != 200:
        refuse(f"the configured key was refused with status {status}: {key_info}")
    account = key_info.get("data", {})
    # `label` is excluded on purpose: OpenRouter's own label embeds a masked
    # fragment of the key, and a receipt has no need of it.
    account_facts = {
        field: account.get(field)
        for field in ("limit", "usage", "limit_remaining", "is_free_tier")
    }

    status, listing = authorized_get(MODELS_URL, key)
    if status != 200:
        refuse(f"account model discovery was refused with status {status}: {listing}")
    live = listing.get("data")
    if not isinstance(live, list) or not live:
        refuse("account model discovery returned no model array")

    with open(arguments.catalog, encoding="utf-8") as handle:
        catalog = json.load(handle)
    described = catalog.get(PROVIDER, {}).get("models", {})
    if not described:
        refuse(f"the public catalog describes no {PROVIDER} models")

    rows = []
    without_metadata = 0
    without_context = 0
    for model in live:
        identity = model.get("id")
        if not isinstance(identity, str) or not identity:
            continue
        if identity not in described:
            without_metadata += 1
            continue
        context = model.get("context_length")
        if not isinstance(context, int) or context <= 0:
            # A model with no stated context window cannot be admitted: the
            # product refuses to fabricate a limit for it.
            without_context += 1
            continue
        rows.append(
            {
                "slug": identity,
                "visibility": "list",
                "context_window": context,
                # No reasoning effort levels are claimed. The account listing
                # does not state them per model in a form this product has
                # verified, and inventing variants would advertise selections
                # that cannot be issued.
                "supported_reasoning_levels": [],
            }
        )
    if not rows:
        refuse("no discovered model is describable by the public catalog")
    rows.sort(key=lambda row: row["slug"])

    root = arguments.service_root
    os.makedirs(root, exist_ok=True)
    discovery_path = os.path.join(root, arguments.discovery_name)
    with open(discovery_path, "w", encoding="utf-8", newline="\n") as handle:
        json.dump({"models": rows}, handle, indent=2)
        handle.write("\n")

    credential_path = os.path.join(root, arguments.credential_name)
    with open(credential_path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(key)
    try:
        os.chmod(credential_path, 0o600)
    except OSError:
        # Best effort; the platform's own source checks still apply.
        pass

    receipt = {
        "campaign": "DAILY-USE-BACKEND-01",
        "acceptance_id": "J01",
        "provider": PROVIDER,
        "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source": {"models": MODELS_URL, "key": KEY_URL},
        "account": account_facts,
        "counts": {
            "advertised_by_account": len(live),
            "described_by_catalog": len(described),
            "admitted": len(rows),
            "excluded_without_catalog_metadata": without_metadata,
            "excluded_without_context_window": without_context,
        },
        "selection_rule": "every account model the shipped public catalog describes, sorted by identity",
        "credential": {
            "written_to": arguments.credential_name,
            "value_recorded": False,
        },
    }
    receipt_path = os.path.join(root, "openrouter-discovery-receipt.json")
    with open(receipt_path, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(receipt, handle, indent=2)
        handle.write("\n")
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
