# Reporting to a StateEngine seat

Grapt can report its work to an external StateEngine seat: the seat shows as
busy while any Grapt session is working, idle when all are done, and each
successful turn is published with its text, model and commit time.

1. Register the seat and connect its bridge, as StateEngine describes for
   external seats: `se agent add NAME --external`, `se mcp --as NAME`, then
   `se external approve CLAIM_ID`.
2. Add the seat to your own Grapt configuration (the service profile's
   `grapt.json` or your user configuration directory):

```json
{
  "stateengine_seat": {
    "home": "/absolute/path/to/stateengine/home",
    "binary": "/absolute/path/to/se",
    "project": "prj_...",
    "agent": "NAME"
  }
}
```

3. Restart Grapt.

`home` is the StateEngine state home that holds `endpoint.json`; `binary` is the
`se` executable it pins. `agent` takes the seat's name or its `ag_` ID. Reports
use your owner credential on this machine, so only your own configuration can
name a seat: a project's configuration that tries is refused.

Without this setting Grapt never contacts StateEngine. With it, turns never
wait for StateEngine. When the host is down, restarts or refuses, Grapt prints a
`stateengine seat warning` line once, skips what it could not report and
reconnects for the next report. A turn that ends while no seat session is known
is not published. Reporting needs Linux today; other platforms print the same
warning and report nothing.
