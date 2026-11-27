#!/usr/bin/env python3
"""Reject absent, partial or cross-candidate PLR custody receipts."""

import argparse
import os
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from custody.receipt import (  # noqa: E402
    CustodyError, candidate_identity, read_manifest, read_receipt, require_clean,
    sha256_file,
)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--id", required=True, help="one exact intended PLR row")
    parser.add_argument("--manifest", required=True,
                        help="the mutation manifest the receipt's harness pinned; "
                             "its row, not the receipt, declares the expected diagnostic")
    parser.add_argument("--collect", help="copy verified bytes to a fresh artifact path")
    args = parser.parse_args()
    try:
        receipt = read_receipt(args.receipt, [args.id], read_manifest(args.manifest))
        current = candidate_identity(Path(__file__).resolve().parents[3])
        require_clean(current, "current candidate")
        if receipt["candidate"]["after"] != current:
            raise CustodyError("receipt candidate differs from current source")
        for name, digest in receipt["harness"]["after"].items():
            if sha256_file(name) != digest:
                raise CustodyError(f"harness changed or is absent: {name}")
        if args.collect:
            source = Path(args.receipt).resolve(strict=True)
            destination = Path(args.collect).resolve()
            destination.parent.mkdir(parents=True, exist_ok=True)
            encoded = source.read_bytes()
            with open(destination, "xb") as handle:
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
            if destination.read_bytes() != encoded:
                raise CustodyError("collected receipt readback differs")
    except (CustodyError, OSError) as error:
        print(f"CUSTODY REFUSED: {error}")
        return 1
    entry = receipt["entries"][0]
    print(f"CUSTODY ACCEPTED: {current['commit']} {args.id} "
          f"executable sha256 {entry['artifact']['sha256']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
