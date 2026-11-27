# StateEngine context in the native terminal

In a Session already bound to StateEngine, Grapt loads continuity before
its first model request and refreshes it after a committed context-window
renewal. Ordinary requests within that window reuse the completed context.
Continuity supplies context; it grants no permission or work authority.

Use `/mcps` to inspect the configured StateEngine connection. Ask for current
working state, relevant recalled memory, a repository map or the impact of a
specific path during ordinary coding. The connected server's tools enter the
same catalog and permission flow as other tools. Repository-map pointers are
not file contents; stale or incomplete impact evidence needs to be checked
against the repository before relying on it. Recalled, unratified memory is
context rather than a project ruling.

If continuity cannot be prepared, the Session keeps a recovery message that
survives a product restart. A missing process capability asks for a rebind; a
connection failure directs you to `/mcps` before retrying. Foreign or
unverified replies remain refused. The model receives neither the capability
nor the private resume cursor.

The StateEngine launcher issues the Session capability, and `grapt session
provision` receives it on stdin and binds it privately to the prepared Session.
New provisioning requires `remote-seat-mail-transport/v2`; retained bindings
without a protocol selector use the v1 compatibility consumer. The configured
`stateengine.native_mail_endpoint` names the loopback mail door; missing or
invalid configuration refuses subscription. See [launching a StateEngine
seat](../daily-driver/STATEENGINE-SEAT.md) for the current R client and launch
contract. Codex courier scripts do not replace the native consumer.

## Retained legacy workflow witness

The walk below uses the retired `grapt-tui` client and synthetic inputs.
It remains a regression fixture; current packaged coding uses `grapt-tui`.

`scripts/stateengine-native/walk.py` copies the built product, CLI and native
TUI into an isolated test package. It uses the existing tmux driver and
loopback provider transport, a scripted model, and a dummy-capability MCP
source. It drives four normal tool approvals and the rebind message after
restarting the product. It checks returned source provenance and stale,
absent impact evidence in the subsequent provider request.

Build the binaries in the same checkout/cache immediately before the walk.
Ordinary binary tests can replace the product artifact; the walk refuses a
product without its loopback testkit marker. Then run:

```sh
cargo build -p grapt-cli --features loopback-testkit --bin grapt
cargo build -p grapt-tui -p grapt-tui-driver
cargo build -p grapt-product --features loopback-provider-testkit --bin grapt-server
python3 scripts/stateengine-native/walk.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" \
  --driver "$CARGO_TARGET_DIR/debug/grapt-tui-driver" \
  --source-commit "$(git rev-parse HEAD)" \
  --evidence /path/to/a/new/evidence-directory
```

In the allocated native interval, run the same command with `--smoke` and a
separate new evidence directory first. Proceed to the full walk only after
the smoke exits successfully. The walk observes the native TUI's exit status
before retiring its retained tmux pane; every run cleans up its owned services.

A successful full walk proves the packaged native client workflow with
disposable sources. It does not prove real StateEngine issuance, provider
acceptance or native mail delivery. No live credentials, profile or paid model is used.

## Real Windows seat launch witness

`real_seat_windows.py` uses a given package and StateEngine checkout, a fresh
`E:/temp/real-seat-*` directory, and private HOME, project, registry and profile.
Run its windowed phase from the desktop after obtaining the foreground go. Do not run it against an existing profile or the shared `secode` project.

Use a new standalone clone of the supplied StateEngine factory checkout, with
a real `.git` directory, the exact candidate checked out, and its own `npm ci`.
Linked author worktrees refuse capture-hook installation across repository
boundaries. This factory-checkout witness includes the job runner; the installed
StateEngine payload omits that factory component, so these results do not
qualify an installed-payload job route.

```powershell
python scripts/stateengine-native/real_seat_windows.py `
  --package E:/temp/seat-package `
  --fixture-binaries E:/temp/seat-fixture-binaries `
  --stateengine E:/temp/stateengine-candidate `
  --stateengine-commit <full-candidate-commit> `
  --node E:/path/to/node.exe `
  --scratch E:/temp/real-seat-unique-run --timeout-seconds 480
```

The CLI and TUI stay unchanged in the package. The separate fixture directory
contains `grapt-server.exe` built from the same source with the existing
`loopback-provider-testkit` feature and its `build-receipt.json`. Build it on
Linux from that exact clean checkout using the shared cache and scheduler:

```sh
source scripts/build-cache.sh
python3 ~/.local/lib/secode-release/heavy.py --nvme --owner seat-fixture-build -- \
  python3 scripts/stateengine-native/real_seat_inputs.py --output /new/fixture-directory
```

The witness verifies the backend's bytes, source, feature, target and build
command against this record before executing it.
Only the model is scripted; the public launch, Session issuance, packet delivery,
MCP calls, mail and job runner remain real. This qualifies their integration
with the fixture carrier; it does not qualify an ordinary release provider or
live model. No system CA or live model credential is installed.

Preparation uses StateEngine's exported `prepareSecodeMcpConfigPath` path-only
API to start the private service with its future generated config path. This
API creates no Session or config bytes; the one real `se seat launch` writes
the config. The run therefore requires that API as well as the public CLI.
The checkout must support the declared private `remote-seat-mail` organ, and
both sides must support `stateengine.native_mail_endpoint`. Do not run an older
consumer that hardcodes the shared mail port against a private engine.

The fixture model verifies the first packet and its exact-principal recorded
acknowledgement, discovers the live MCP tool through the default CodeMode
catalog, and reads mail through that tool. After durable idle, the private human
sends a message. The seat reads and explicitly acknowledges it through MCP,
starts `se job start --` for a short job
that exits 3, and becomes idle again. A new automatic input must contain that
job's exact ID, exit code and log path; polling output cannot satisfy it.
Afterward, the product must keep its subscription open for two idle minutes,
without a reconnect, drop, unavailable notice or process exit. The witness
records the original product log lines when this fails. This checks idle
subscription stability; it does not inject a failure before the provider starts
or qualify repeat-wake behavior after that failure.

`assertions.txt` and `assertions.json` give nine PASS/FAIL results and their
exact lines in `observations.jsonl`. Missing observations fail. `result.json`
also requires completion and successful cleanup of the owned seat, service and
private engine. The execution deadline is 120–600 seconds; cleanup has a
separate bounded timeouts. Retain this private directory for diagnosis;
do not commit its generated configs, credentials or logs.

Run the same current pair with `--fault config-trust`,
`--fault mail-endpoint-missing` and `--fault job-project-missing`, using a fresh
scratch directory each time, then repeat `--fault none`. Each fault run must
exit unsuccessfully and its `control.json` must report `detected: true`; the
clean run must pass again. An unconfigured connection cannot satisfy the
blocked-source control.

All runs use the same forwarding proxy to observe real Session creation. Before
forwarding that first write, the trust control copies the generated config
unchanged into a throwaway git repository and the endpoint control removes only
`stateengine.native_mail_endpoint` from the actual generated config. This
barrier prevents native mail from subscribing before a fault takes effect.
The job shell invokes the supplied Node and `bin/se.mjs` paths, with the
supported explicit workspace, private-registry and CLI-project environment
bindings. The job control removes only the owner-project variable
`STATEENGINE_PROJECT_ID` in that child shell;
its public start receipt must say that owner-project attribution is off, and
the idle seat must receive no matching completion wake. Wrong-seat attribution
with a valid Session has no supported override and is not claimed.
