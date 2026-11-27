# Resident headless launch contract

A resident client observes one service-owned Session across idle periods:

```sh
grapt run --service PROFILE --session SESSION --stdin --jsonl --resident
```

This is a contract for any launcher, not a particular host implementation.
`--resident` does not daemonize, create a service, or transfer ownership of a
process. The service must already exist independently of the launching process.
Do not use an embedded service when that service must outlive its client.

## Binary and initial input

Resolve the intended installed binary before spawning and retain its identity.
Invoke it directly with an argument vector, not by concatenating a shell command.
A launcher may resolve it through PATH, but must not silently switch builds during
an existing launch.

`--stdin` reads one bounded UTF-8 prompt through EOF before connecting. Write the
initial prompt, close the write end, and do not hold it open as a keepalive. EOF
ends the prompt, not the resident attachment. Later work arrives through the
service's authenticated prompt/mail APIs; the client does not replay its initial
prompt at idle. Provision the Session's authenticated mail binding before launch.

## Process lifetime and output

The launcher owns detachment. On Linux, separate the client from a launcher-owned
terminal/session or process group that will be closed or killed on launcher exit.
On Windows, provide independent process/console lifetime and do not leave the
client in a kill-on-close job whose last owner is the short-lived launcher.
A recorded failure to detach is not proof that the client will survive.

Prefer stdout/stderr files whose lifetime is independent of the launcher, or an
output consumer supervised independently of it. While attached, stdout is bounded
JSONL and stderr carries diagnostics. Drain both pipes; a reader that remains
open but stops consuming can block writes. Preserve exit status and stderr in a
place the host can still inspect after the launch command returns.

If a resident stdout reader closes, the client abandons that stream and continues
observing the same Session and maintaining its approval attachment. It does not
retry a partial JSONL record, redirect records into stderr, or readmit work. The
lost output is available through the service's message APIs, not by reopening the
old pipe. Foreground commands without `--resident` still fail on a broken pipe.
Other output errors and output bounds remain errors; closing output is not a
request to stop work.

## State and takeover

Use authenticated service queries for execution state and durable results:

- `GET /api/session/active` distinguishes active executions from idle Sessions.
- `GET /api/session/SESSION/message` reads durable messages and tool outcomes.
- `GET /api/session/SESSION/permission` reads pending approval requests.

An idle Session is not a dead client, and a live service is not proof that its
resident client is alive. Retain the exact client process identity, monitor its
exit independently of JSONL, and report an unexpected exit with its exit status
and captured diagnostic. If both pipes were owned by a process that died, their
absence cannot certify a successful client exit.

Before takeover, prove that the previous exact client is absent and reconcile
any active service execution through the authenticated service API. Do not infer
absence from lost output, start a second client merely because the Session is
idle, or delete the Session to conceal unfinished work.

## Requested shutdown

Send supported cancellation to the exact client: SIGINT/SIGTERM on Linux, or
Ctrl+C/Ctrl+Break through its independently owned console/process group on
Windows. The CLI requests authenticated interruption of its Session and waits
for the existing bounded cancellation result. Observe both process exit and the
service's execution outcome; a kill request alone proves neither. Keep an output
reader draining through shutdown when it is still attached.

The client does not stop a separately owned service. Stop that service separately
only when the host owns its lifetime. If forced termination is necessary after a
grace period, report it as forced and reconcile the service execution before a
replacement. No program can report its own SIGKILL, job teardown, or machine loss;
the host's independent process supervision is required for those cases.

## External-host service startup

Probe the selected server image before changing an incumbent:

```sh
grapt-server --external-host-capabilities
grapt-server init-profile --service PRIVATE_PROFILE
grapt-server serve-service --service PRIVATE_PROFILE --port 0 --external-host
grapt status --service PRIVATE_PROFILE --jsonl
```

The host reserves and protects `PRIVATE_PROFILE`, supervises the service separately
from resident clients, and retains cleanup custody from creation. To import provider
configuration into a fresh profile, use `init-profile --service PRIVATE_PROFILE
--from PROVIDER_PROFILE`; never point bootstrap at a real profile's database to
create a new service. The ordinary connection-setup APIs remain available when a
profile has no provider yet. Select and read back the Session's model, permissions,
workspace and instructions before starting its resident client.

Scrub `STATEENGINE_*`, `SECODE_STATEENGINE_*` and `SECODE_SESSION_ID` from the
service child environment. Direct server startup refuses inherited launcher
identity. Explicitly provision each Session's own capability rather than inheriting
the launcher's. External mode does not start the legacy native-mail consumer or
an embedded StateEngine owner; configured MCP transports remain configured by the
profile, so declare the standalone runtime's intended transport explicitly.

`status` authenticates readiness, health and registration together. Record
its service instance, generation and endpoint. Authenticate HTTP controls with the
private profile's bounded `basic.credential` (`username:secret`, optional trailing
newline); never put that secret in argv or diagnostics. A matching PID or an
existing readiness file alone is not service authority.

Stop the exact independently owned service with SIGINT/SIGTERM on Unix or Ctrl+C
on Windows, then observe exit and registration retirement. There is no service
shutdown HTTP route. Windows service Ctrl+Break and no-console cancellation are
not advertised by this interface; a host without a qualified Ctrl+C path must
refuse that launch rather than substitute forced termination as graceful shutdown.

## Keyed initial admission and readback

Reserve one original `MessageId` and pass it on the real client invocation:

```sh
grapt run --service PRIVATE_PROFILE --session SESSION --stdin --jsonl --resident --input-id msg_initial
```

The key requires an existing exact `--session`, not `--new`, `--resume` or a fork.
The CLI sends it as `id` on its single initial prompt admission and verifies both
acknowledged ID and Session. Keyed startup uses `delivery: "queue"` and
`metadata: {"origin": "seat-startup"}`; it does not clear a human-stop hold on
native work. Do not pre-admit a prompt and then launch an unkeyed client.
Closing stdin proves only that the prompt bytes were sent, not that they were
admitted. The authenticated, read-only acceptance lookup is:

```text
GET /api/session/SESSION/input/msg_initial
```

The response has `service` (`incarnation`, `serviceInstance`, `generation`,
`registrationVersion`) and nullable `data`. The incarnation is fresh for every
service application lifetime, including a clean stop and restart of the same
profile; configured registration fields alone do not distinguish those lifetimes.
Retain the incarnation from authenticated host-control readback before admission.
Present data carries the original
`id`, `sessionID`, `admittedSeq`, `timeCreated`, `kind`, `text`, `delivery` and
`metadata`, whether the input is still pending or already promoted. Compare all
original service/Session/key and prompt coordinates before treating it as an
acceptance. This proves durable admission, not execution completion or a live
client. A successful lookup with `data: null` establishes no matching admission
in that exact observed service; it is not permission to resend an uncertain write
that could still be in flight. Unreadable, conflicting, changed-generation or
partial readbacks remain pending/no-replay. Reconcile the original operation;
never mint a replacement key to conceal uncertainty.

The underlying prompt API still accepts explicit `id`, `delivery: "queue"` and
`resume: false` for admit-only callers. POST reconciliation is not a readback: it
can advise execution. Public message history alone does not cover pending input.

## Authenticated native-notice control

After writing the Session's `stateengine-identity` instruction and provisioning
`PUT /api/session/SESSION/stateengine/capability`, read:

```text
GET /api/session/SESSION/stateengine/notice
```

It returns `service`, `sessionID`, nullable `targetID`, nullable binding fingerprint
`binding`, nullable `holdID`, and `status`: `ready`, `held` or `rebind-required`.
The target changes when capability/binding custody is replaced. `ready` describes this service's
notice consumer, not resident-process liveness or model consumption. The host
must independently observe its exact resident client and authenticated binding.

Offer only committed source message IDs, with an original notice key distinct
from the initial prompt key:

```text
POST /api/session/SESSION/stateengine/notice
{
  "id": "msg_notice",
  "targetID": "evt_target_from_readback",
  "binding": "binding_fingerprint_from_readback",
  "messages": ["source_message_id"],
  "incarnation": "incarnation_from_readback",
  "serviceInstance": "instance_from_readback",
  "generation": 1,
  "registrationVersion": "version_from_readback"
}
```

Use the exact recorded service identity, including its incarnation, not the
illustrative generation above.
The bounded list contains 1–32 distinct opaque source IDs, each at most 256 UTF-8
bytes. No message bodies or caller-authored instruction text are accepted. The
service verifies its generation and the Session's authenticated capability and
current native target, then durably admits a separate synthetic notice through
the existing input queue. Active work sees it at an eligible safe boundary; idle
work is awakened by the same service-owned driver. The model reads message bodies
through its bound authenticated inbox.

The response uses the same durable acceptance shape as the input lookup. Reconcile
uncertain notice delivery with `GET /api/session/SESSION/input/msg_notice`, retain
its original key, and compare the notice metadata and target. Exact retries cannot
create another admission or move an old notice to a new binding target. Stop and
revocation fences keep held or superseded native work from automatically resuming;
capability reprovisioning alone does not undo a human stop. An admission receipt is
not an acknowledgement that the model read or answered the source messages.

## Explicit native-mail resume

A host acting on an explicit human resume may release a native-mail stop without
replaying the initial prompt or submitting replacement input. First reconcile the
prior interruption, prove the old resident client is absent, and observe settlement
of its execution; this control is not a way around uncertain execution custody:

```text
POST /api/session/SESSION/stateengine/resume
{
  "holdID": "hold_from_held_notice_readback",
  "targetID": "target_from_held_notice_readback",
  "binding": "binding_from_held_notice_readback",
  "service": {
    "incarnation": "incarnation_from_readback",
    "serviceInstance": "instance_from_readback",
    "generation": 1,
    "registrationVersion": "version_from_readback"
  }
}
```

Copy the exact authenticated notice readback coordinates, including its `holdID`,
only after observing `status: "held"`. This operation checks current capability
custody and atomically releases that exact hold on that exact selected target.
It cannot restore a revoked target, replace capability custody, or clear a later
stop. A stale request returns conflict, including a retry after release; never
replace its coordinates automatically to defeat a newer stop.

Success returns the notice-binding readback. Reconcile a lost response with
`GET .../stateengine/notice`: matching service/target/binding with `ready` means
the native hold is released; `held` or changed coordinates require reconciliation
with the human before another release. No input is admitted or promoted, and no
execution is started by this control. Once ready, reconcile an existing pending
notice using its original notice key and exact payload, or offer newly committed
source IDs through the ordinary notice control. Do not resend the initial prompt.
Observe the resident client and execution separately; a released hold proves
neither liveness nor model consumption.
