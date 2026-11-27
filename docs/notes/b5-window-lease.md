# B5: the window lease

Close means closed: a Session must not keep running and spending tokens after
the last window that can watch it is gone.

This note records the design that the F23 window-close change implements, the
parts that are still open, and what the evidence does and does not show. It
takes the four required changes from review and the three close semantics of
design note v2 §4.4 as given.

## The lease

A window holds a lease on a Session through its named event subscription.
`grapt-api`'s `WindowWatch` keeps, per Session, the set of live window claims;
a claim is released when its `SseBody` drops. The service therefore decides
"the last window is gone" from its own registry, never from a window's report:
a crashed window announces nothing, and its subscription ending is the only
truthful signal.

A window names its Session on `/api/event?session=…`, on every subscribe and on
every resubscribe, so a reconnect keeps its lease. A Session launched to run
without a window registers no lease and never starts a grace.

## Three ways a lease ends

1. **Deliberate close** — quit, console close, `SIGHUP`/`SIGTERM`/TTY EOF. The
   window sends the ordinary Session stop before it exits, and the turn stops
   at once. No grace is involved: the stop is a fact, not an absence.
2. **Silent loss** — a killed window, a power loss, a dropped tunnel. The
   subscription simply ends. The service starts a grace and stops the turn only
   when it runs out; a window that returns inside it cancels the stop.
3. **Handover** — an upgrade or relaunch that keeps the Session. The outgoing
   window detaches with a handover window, during which the ordinary grace does
   not run. A replacement that attaches cancels the hold; one that never
   arrives falls back to the silent-loss grace rather than holding forever.

## Grace: how long, and measured how

- The default silent-loss grace is **30 s**, and a deployment may configure it
  with `close_grace_ms` in `service.conf` (bounded to 1 ms–24 h). Remote windows
  sit behind tunnels, so a tight grace would stop turns whose window was merely
  slow to reconnect.
- The grace is measured on the **monotonic clock**, so a wall-clock jump cannot
  expire a lease early.
- It is **stall-aware**. If the service's own loop is blocked past a tick, the
  elapsed time belongs to the stall, not to the missing window: the registry
  rebases every running grace from the moment the loop recovered. A
  service-side stall therefore never kills turns.
- A stall-aware grace still cannot be measured while the loop is not running,
  which is why the **keepalive heartbeat** matters: a half-open connection (a
  sleeping laptop, a dead network) keeps its socket until the TCP timeout, so
  without one its lease would never end. Two of the three halves are in place
  and one is still open:
  - The service emits an idle `": keepalive"` frame every 15 s
    (`api/src/stream.rs:17`, `:105-111`). That is one direction only: it keeps a
    proxy from dropping an idle connection and tells the service nothing when a
    peer that is gone ignores it.
  - The **write side** is bounded. Each write batch arms a 30 s allowance
    (`api/src/stream.rs:18`, `:140-155`); when it elapses the write fails, the
    connection ends, and dropping the response body releases the `WindowClaim`
    that body holds, which detaches the window and wakes the lease owner
    (`api/src/u3a/sse.rs:800-825`). The socket adds TCP keepalive probes and,
    where the platform exposes it, `TCP_USER_TIMEOUT` at the same allowance
    (`api/src/stream/transport.rs:40-56`).
  - What is still missing is a **client heartbeat the service requires**, so a
    peer that is alive but silent can be told apart from a slow one. That is
    deferred until remote connections are real.

## Headless is a property of the launch

Whether a Session may run unwatched is a fact about how it was launched, not
about who happens to be subscribed. A window that attaches to a headless-launched
Session and then closes must not stop it.

The launch mode is recorded durably as a `session.launched` fact written in the
same transaction as the Session's seed, so a Session never exists without the
mode its launcher stated. It is its own row rather than a member of the frozen
seed object, which is what lets an older build skip the row and still read the
Session (see the rollback regression in
`crates/grapt-session/tests/launch_fact_rollback.rs`).

A launcher states the mode on a header, `X-Grapt-Launch: headless|interactive`,
which keeps the frozen create body unchanged. An absent header is absence, not
`interactive`: a create that never stated a mode writes no fact, and the service
decides once, when that Session's first prompt arrives, from whether any window
holds a lease on it. That is the older clients' fallback. An unrecognized header
value is refused rather than defaulted, because silently reading a headless
launch as windowed would arm the grace against a Session meant to run alone.

The stop decision reads the mode from a cached snapshot, never from the store:
the deadline loop must not block on a read, or one slow store would stall every
other Session's grace.

## After a restart

A restart ends every subscription at once, so every lease is gone. The registry
starts empty. Two things must then hold, and only the first is implemented here:

- A window that reattaches inside the grace re-establishes its lease, so the
  turn survives a restart that a window outlives. Implemented: leases are
  process-local and reattach on subscribe.
- A Session whose turn was running or resumed when the service restarted gets a
  **pending silent-loss grace**, so a turn whose window never returns does not
  run unseen forever. **Not implemented**: it needs a durable enumeration of
  turns owed at startup, which this branch does not add. Until then a restart
  with no returning window leaves the turn running.

## What the evidence shows

End-to-end journeys in `crates/grapt-product/tests/dogfood_headless/close_grace.rs`,
each driving a real service, a real HTTP window and a scripted provider:

- `closing_the_last_window_stops_the_turn_and_no_provider_call_follows`: a
  silent loss stops the turn, no further provider call arrives, and input queued
  behind the turn is left queued rather than promoted.
- `a_session_with_two_windows_keeps_running_until_the_last_one_goes`: one of two
  windows closing changes nothing; the last one stopping does.
- `an_explicit_close_stops_the_turn_before_the_grace_expires`: an explicit close
  stops the turn well inside the grace, so idle cannot be the grace expiring.
- `an_explicit_close_does_not_promote_queued_input`: admitted input stays queued
  after a close.

The queued-input assertion fails on the previous stop path (which only signalled
a registered execution) and passes on the complete one, so it discriminates.

Unit tests in `grapt-api` cover the registry itself: only the last window
counts, a returning window cancels, an unknown name is not evidence, a resumed
window is one claim, a fresh loss does not hide an expired one, a configured
grace replaces the default, a stall rebases a running grace, and a handover
holds for its replacement and falls back when none arrives.

## Open, in dependency order

1. Keepalive heartbeat. Partly implemented: the service's idle frame, the write
   allowance that ends a stalled stream and detaches its window, and the socket
   keepalive probes are all in place (see above). What remains is a **client
   heartbeat the service requires**, so a peer that is alive but silent can be
   told apart from a slow one; that is deferred until remote connections are
   real.
2. ~~The launch mode recorded on the Session~~ — implemented: a durable
   `session.launched` fact written with the seed, stated on `X-Grapt-Launch`,
   read by the stop decision from a cached snapshot, with the older clients'
   window-presence fallback.
3. The pending grace for turns owed at startup after a restart.
4. ~~The Unix signal handlers that turn a hangup into a deliberate close~~ —
   implemented: `grapt-console/src/listener/unix.rs` registers `SIGHUP` and
   `SIGTERM`, `console/src/lib.rs:63` maps them to `Intent::Close`, and
   `tui/src/main.rs:271` takes the same path a Windows console close takes.
   Two parts of the original wording need separating.
   - **Redirected stdin** cannot inject an end of file: the input owner opens
     `/dev/tty` directly (`tui/src/startup_input.rs:164`) and a failed open ends
     startup, so a closed descriptor 0 or a redirected stdin never reaches the
     reader. That much holds.
   - **A terminal hangup can.** An already-open raw terminal returns EOF, or
     fails the read with `EIO`, once its peer is gone; verified by hanging up a
     PTY from the master side and reading the child's `/dev/tty`, which failed
     with errno 5. So the reader's `count == 0` arm (`:198`) and its read-error
     arm are both reachable, and the earlier claim that the criterion was
     impossible was wrong.
   What remains open is behavioural, not reachability: that error reaches the
   input queue and `tui/src/main.rs:311` treats it as a status line
   (`ui.status = error`) with no `close_console`, so a hangup delivered this way
   ends the turn only when the lease grace expires. Whether it should request
   the same deliberate close the signals do is its own assessment, with its own
   witness, and is not settled by this note.
