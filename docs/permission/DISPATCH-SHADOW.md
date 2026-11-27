# Tool dispatch observations

The service writes bounded, observational records to
`diagnostics/tool-dispatch-shadow.jsonl` under its profile. This ledger changes no
permission decision, tool result, recovery preparation or retry. It makes no model
calls. Ephemeral sessions do not create its writer.

Each product-runner entry gets a host-generated ledger instance and dispatch
ordinal. Records distinguish direct tools, CodeMode children and plugin host
operations. A CodeMode envelope is labeled separately from its concrete children.
Session and Physical Attempt identities connect observations to existing usage;
an unavailable attempt stays null. Provider/tool call IDs are never logged.
Each coordinate has a 256-byte observation bound. An oversized session or attempt
is omitted with `coordinates_over_bound: true`; dispatch counting remains possible
but its missing association must stay explicit.

The phases are runner dispatch, preparation after input hooks, permission granted,
effect started, and terminal outcome. Dropping an unfinished dispatch records
`abandoned`. Earlier runtime schema/capacity refusals never entered this runner;
their absence is not a claim that every incoming request was admitted. Permission
failure and effect start remain separate observations.

Tool and command shapes use closed labels. Arguments, command text, script bodies,
paths, arbitrary external tool names, outputs, secrets and their lengths are not
retained. Oversized or incompletely parsed commands have an explicit unknown
shape. Every record says `routing: unmeasured`: these are observed lifecycle
events, not proposed allow/block/guard decisions or evidence of guard correctness.

The existing diagnostic writer has a 256-line queue and a 1 MiB file cap,
including retained data after restart. Offering an observation never waits for
disk. Queue losses appear as `dropped_before` and sequence gaps; a cap marker
means later events were not recorded. An unavailable writer or file can lose the
entire observation stream without changing execution. Missing records therefore
cannot establish a complete dispatch denominator. Count dispatch ordinals within
their ledger instance, and report losses, cap markers and missing coverage.

This is the first observational slice. Several days of fleet measurements,
separately authorized guard-call experiments, and independent red-team findings
and corrections still precede any new enforcement.
