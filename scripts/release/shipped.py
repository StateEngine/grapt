"""The SHIPPED SET: which binaries a release package contains, and what each is.

## Why this is one file instead of two lists

The builder decides what to compile and the packager decides what to accept. If
those two answers live in two places they can disagree, and the way they
disagree is silent: a builder that stops producing a binary simply produces
four, and a packager that only checks "is every packaged binary accounted for by
a build command" packages four and says nothing. Every field it emits is true.
The package is just missing something, which is the one defect a provenance
record built entirely out of true statements cannot catch.

So the set is declared ONCE, here, and both sides read it. The builder compiles
exactly this; the packager requires exactly this, refusing a missing entry and
an unexpected extra with equal force.

## Where the membership came from

Enumerated from `cargo metadata --no-deps`, not from a grep for `[[bin]]`. A
grep finds only explicit declarations and silently misses every auto-bin --
which is precisely how `grapt-engined` was absent from the first version of
this list.

Development tools are deliberately EXCLUDED
and named here rather than omitted, so that "not shipped" is a recorded decision and not
an oversight that looks identical to one.

## Why roles are here and not only in prose

An operator recovering a failed activation reads the manifest, not this file's
docstring. The packaging disposition requires the manifest and the operator
recipe to disclose that `grapt-engined` is auxiliary -- it seals an EMPTY
router and serves no product route, not even `/health`, and its `service
ensure|stop` refuse by design with a typed non-zero refusal. Shipping it without
that disclosure would let an operator reach for it as the server. Carrying the
role beside the name is what makes the disclosure travel with the artifact.
"""

from __future__ import annotations

#: Produced binary -> its declaration. `package` and `bin` are the cargo
#: coordinates; `role` and `disclosure` are what the manifest and the operator
#: recipe must say about it.
SHIPPED: dict[str, dict[str, str]] = {
    "grapt-server": {
        "package": "grapt-product",
        "bin": "grapt-server",
        "role": "product-server",
        "disclosure": "the product server; this is the binary a service runs",
    },
    "grapt": {
        "package": "grapt-cli",
        "bin": "grapt",
        "role": "cli",
        "disclosure": "the command-line / ACP client",
    },
    "grapt-tui": {
        "package": "grapt-tui",
        "bin": "grapt-tui",
        "role": "terminal-client",
        "disclosure": "the native terminal frontend launched by grapt and grapt code",
    },
    "grapt-migrate": {
        "package": "grapt-release",
        "bin": "grapt-migrate",
        "role": "migration-operator",
        "disclosure": (
            "the migration and rollback operator. REQUIRED as a delivered "
            "artifact: the situation it exists for is a failed activation on an "
            "operator's machine, where there is a broken install and a backup "
            "but no Rust toolchain, no workspace and no target directory. A "
            "rollback reachable only through `cargo run` inside a checkout is a "
            "developer convenience spelled like a recovery path"
        ),
    },
    "grapt-engined": {
        "package": "grapt-engined",
        "bin": "grapt-engined",
        "role": "auxiliary",
        "disclosure": (
            "AUXILIARY headless composition/status tool. NOT the product server "
            "and NOT a recovery controller: `production_service` seals an "
            "ApiRouterBuilder with nothing registered, so it serves no product "
            "route, not even /health, and `service ensure|stop` refuse by "
            "design with a typed non-zero refusal naming the missing authority. "
            "Never on an install or recovery success path; `grapt-server` is "
            "the product server"
        ),
    },
}

#: Binaries that exist in the workspace and are deliberately NOT shipped, with
#: the reason. Named rather than omitted: an excluded artifact and a forgotten
#: one look identical in a list that only carries what is included.
EXCLUDED: dict[str, str] = {
    "cache-usage-benchmark": "a measurement tool, not a product artifact",
    "grapt-tui-driver": "a PTY acceptance driver, not a product artifact",
}


def cargo_coordinates() -> dict[str, tuple[str, str]]:
    """`{binary: (package, bin)}` — what the builder compiles."""
    return {name: (entry["package"], entry["bin"]) for name, entry in SHIPPED.items()}


#: Executable suffix per target. A Windows cross-build produces `.exe` names,
#: and the shipped set is the same set either way -- so the suffix is applied
#: HERE, at the one place that knows the membership, rather than being stripped
#: and re-added by every consumer that compares two lists.
SUFFIX: dict[str, str] = {
    "x86_64-unknown-linux-gnu": "",
    "x86_64-pc-windows-msvc": ".exe",
}


def suffix_for(target: str) -> str:
    """The executable suffix for `target`, refusing an unknown one.

    Defaulting an unknown target to "" would silently produce a Linux-shaped
    expectation for a target nobody declared, and the failure would surface as a
    missing binary rather than as the unknown target it is.
    """
    if target not in SUFFIX:
        raise KeyError(
            f"no executable suffix is declared for target {target!r}; "
            f"declared targets are {sorted(SUFFIX)}"
        )
    return SUFFIX[target]


def names(suffix: str = "") -> list[str]:
    return sorted(f"{name}{suffix}" for name in SHIPPED)


def roles(suffix: str = "") -> dict[str, dict[str, str]]:
    """`{binary: {role, disclosure}}` — what the manifest must say."""
    return {
        f"{name}{suffix}": {"role": entry["role"], "disclosure": entry["disclosure"]}
        for name, entry in SHIPPED.items()
    }
