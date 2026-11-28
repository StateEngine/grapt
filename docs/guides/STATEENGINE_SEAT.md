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
use your owner credential on this machine, so only your own configuration is
read for a seat: a configuration file that is a link, or a service profile
that lives inside a repository, is refused.

Without this setting Grapt never contacts StateEngine. With it, turns never
wait for StateEngine. When the host is down, restarts or refuses, Grapt prints a
`stateengine seat warning` line once, skips the turns it could not publish and
keeps retrying until the seat shows Grapt's current busy or idle state. A turn
whose start Grapt did not see, such as one already running when Grapt
reconnected to its own event feed, is not published. Reporting starts once
the service is ready. When Grapt stops, the seat is reported idle; Grapt waits
at most a second for that report. Reporting needs Linux today; other platforms print the same
warning and report nothing.
