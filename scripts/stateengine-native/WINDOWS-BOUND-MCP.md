# Windows bound MCP journey

`windows_bound_mcp.py` tests the real canonical Windows producer and R console in
an exclusively created profile and workspace. The model uses labelled loopback
fixture binaries; this is not a paid provider or seat-day measurement.

The checks cover the literal `inbox`, multiword and `skill` searches, real resume,
an oversized-list refusal, a subsequent small list, and a durable send confirmed
independently through the producer's public human inbox. `skill` remains a built-in
tool rather than a deferred MCP registration.
The first actual provider request must contain the delivered packet and its
matching recorded acknowledgement; the explicit resume must acknowledge it too.

Prepare same-source `grapt.exe`, `grapt-server.exe` and `grapt-tui.exe` with
the existing `grapt-cli/loopback-testkit` and
`grapt-product/loopback-provider-testkit` features. Put their full source commit
in `source.txt` beside them. Supply the complete Windows StateEngine payload
selected by the repository's C10 pin. Missing or mismatched input fails before
producer execution; do not substitute an unpinned standalone supplier.

Read the installed bench kit's `PC-LOCK.md` and check `pclock.py show` before
acquiring the PC. Obtain the independent readiness ACCEPT, then the attended
GO and reservation before this visible invocation:

```text
python windows_bound_mcp.py --binaries E:/temp/bound-mcp-binaries --source FULL_COMMIT --payload PINNED_PAYLOAD --scratch E:/temp/bound-mcp-UNIQUE --bench-kit E:/temp/kappa-perf-bench02-20261024 --go-msg msg-ACTUAL_GO --readiness-accept msg-ACTUAL_ACCEPT --expires-unix HARD_DEADLINE
```

The controller uses the installed lease and 32-process kill-on-close job before
preparation. It starts one canonical windows-console consumer. Console input and
capture use handles for the verified private executable, service and Session;
they do not use desktop focus or global keyboard input. Cleanup retains the R
process handle, stops the canonical seat and private engine, verifies that the
console closed, then removes the disposable issuer. A bounded final kernel job
census must contain only the controller before lease release. A cleanup or census
failure fails the journey and retains the lease until process exit kills the job.

`result.json`, the launch receipt and viewport captures stay in the private
scratch directory. Any concurrent live load is recorded as a condition;
its profile, package and windows are never modified.
