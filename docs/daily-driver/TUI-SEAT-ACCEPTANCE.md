# Native TUI seat acceptance with disposable peers

This companion to [headless seat acceptance](HEADLESS-SEAT-ACCEPTANCE.md) operates
the real native client under a bounded tmux PTY. It uses synthetic packets and
local model, MCP and native-mail peers, with no real StateEngine principal or
paid provider. The scratch directory must be new and port 4877 must be free.

```sh
python3 scripts/stateengine-native/tui_acceptance.py \
  --cli /absolute/test-binaries/grapt \
  --product /absolute/test-binaries/grapt-server \
  --tui /absolute/test-binaries/grapt-tui \
  --driver /absolute/test-binaries/grapt-tui-driver \
  --scratch /absolute/new-evidence-directory
```

Use a separately staged product built with `loopback-provider-testkit`. An
ordinary release product is refused. Stage the intended package's CLI and TUI
with a matching-source testkit product when repeating this journey after a
package refresh. This checks those clients with fixture transport; it does not
establish acceptance of the release service or a live producer seat.

The journey checks verified initial packet context, idle-mail full read/reply,
mail published during a held provider request, an unsent composer draft,
`/compact` followed by a coding request carrying the actual summary, and mail
published while the service is stopped. After restart, the client must show the
missing-capability refusal; explicit capability rebinding and packet redelivery
then permit exactly the new mail. Final model and MCP counts are checked after
the TUI and product have exited cleanly, including late completions.

Text enters through the terminal's bracketed-paste protocol and a separate
submit key. Captures wait for visible text; an immediate snapshot is not proof
that queued input has been painted. The busy fixture records whether any MCP
mail effect crossed the held provider step. The final check requires all three
messages to have exactly one full read and one reply in order.

Startup waits for the catalog refresh to finish with the saved agent and model
visible. The editable composer appears earlier; its footer alone does not mean
those selections are ready to submit. The journey makes no picker intervention
or selection write. A passing result records
`prepared_launch_without_intervention: true`.

Screens, request captures, public history and peer operations remain under the
new scratch directory. Failed journeys retain `passed: false`. These observations
do not establish cache performance, Windows acceptance or a full seat-day.

Run fixture refusal and continuity checks with:

```sh
python3 -m unittest discover -s scripts/stateengine-native -p 'test_*.py'
```
