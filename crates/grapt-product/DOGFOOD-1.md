# LIVE-HEADLESS-1 headless journey

Run the complete real-process witness from the workspace root:

```text
cargo test -p grapt-product --features loopback-provider-testkit --test dogfood_headless -- --exact live_headless_wire_failure_and_resurrection_witnesses --nocapture
```

The witness launches the shipped `grapt-server serve` against a disposable
migrated store. The production composition preserves the operator credential,
issues the selected model through the catalog authority, and executes it through
the production provider and runtime. The test-only feature substitutes a
controlled loopback endpoint, not an alternate execution path.

The witness observes bearer/account authentication, real request shape,
reasoning/text/usage SSE, typed authentication and malformed-stream failures,
interruption, durable re-list, and byte-equivalent restart replay with no second
provider request. LIVE-HEADLESS-1 deliberately excludes tools, attachments,
subagents, campaigns, desktop work, and billable external requests.
