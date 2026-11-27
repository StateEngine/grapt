"""Explicit acceptance features; production builds keep their ordinary defaults."""

FEATURES = {
    "grapt-cli": ("loopback-testkit",),
    "grapt-connections": ("loopback-testkit",),
    "grapt-login": ("loopback-testkit",),
    "grapt-mcp-client": ("loopback-testkit",),
    "grapt-product": ("checkpoint-fixture", "handover-crash-fixture",
                       "loopback-provider-testkit"),
    "grapt-provider": ("deterministic-testkit", "loopback-testkit"),
    "grapt-runtime": ("loopback-testkit",),
}


def arguments(packages=None):
    selected = FEATURES if packages is None else set(packages)
    features = [f"{package}/{feature}" for package in sorted(selected)
                for feature in FEATURES.get(package, ())]
    return ["--features", ",".join(features)] if features else []


def ordinary_commands(packages=None):
    """Keep the two handover tests excluded by the crash-fixture feature."""
    if packages is not None and "grapt-product" not in packages:
        return []
    return [["cargo", "test", "--locked", "-p", "grapt-product",
             "--test", "handover_journey", "ordinary_binary_ignores_the_fixture",
             "--no-fail-fast", "--", "--test-threads=4"]]
