# `grapt run`: server-loss and cancellation contract

WINDOWS-CLI-SERVER-LOSS-01 J02. This covers `grapt run` (the executor's `run_with_cancellation`)
on Windows and Linux. Every bound below is derived from a named constant in `crates/grapt-cli`.
None of them is a tuned number. The evidence is in [RESULTS.md](RESULTS.md).

## Constants the contract is built from

| Constant | Value | Where | Meaning |
| - | - | - | - |
| `REQUEST_ALLOWANCE` | 30 s | `client.rs` | Connect timeout and total timeout for every request, the event stream (SSE) included. reqwest 0.13 applies a total timeout "from when the request starts connecting until the response body has finished". |
| `TERMINAL_POLL_INTERVAL` | 1 s | `run.rs` | The run loop's idle tick. Each tick issues a permissions re-list and an active-Session read. |
| `SERVER_LOSS_EXIT_BOUND` | 31 s | `run.rs` | `TERMINAL_POLL_INTERVAL + REQUEST_ALLOWANCE`. Time from the loss to the first counted transport failure. |
| `MAX_CONSECUTIVE_TRANSPORT_FAILURES` | 5 | `client/stall.rs` | Consecutive transport failures a durable re-read tolerates before the run ends. |
| `MAX_TRANSPORT_STALL_TIME` | 150 s | `client/stall.rs` | `REQUEST_ALLOWANCE x MAX_CONSECUTIVE_TRANSPORT_FAILURES`. The total request time plus backoff a streak may consume. |
| `TRANSPORT_STALL_EXIT_BOUND` | 151 s | `run.rs` | `TERMINAL_POLL_INTERVAL + MAX_TRANSPORT_STALL_TIME`. The bound from server loss to a stalled exit. |
| `CANCELLATION_HANDOFF` | 1 s | `run.rs` | `TERMINAL_POLL_INTERVAL`. How long a stopped run has to hand its output back. |

## 1. Server loss, while stdout and stderr accept writes

**A run whose server is lost exits Remote (12) within `TRANSPORT_STALL_EXIT_BOUND` (151 s) of
the loss.** "Lost" covers a killed or crashed process, a host that is gone, and a server that
goes silent after admitting the prompt. The durable re-reads — the permission re-list, the
active-Session read and the message re-reads the run loop issues after every tick and advisory
end — are re-reads of durable state, never new effects, so a transport failure on one reissues
it: the run tolerates up to `MAX_CONSECUTIVE_TRANSPORT_FAILURES` (5) consecutive transport
failures on the service before it ends, and the run's final error then names the service, the
failure count and the last transport error.

**Derivation.** The bound is the time from the loss to the fifth consecutive failed re-read:
- At the moment of loss the loop is in one of two states:
  - **A request is in flight.** It ends within `REQUEST_ALLOWANCE` of its own start, which is at
    most 30 s after the loss.
  - **The loop is idle, waiting on the event stream and its tick.** If the event stream fails, a
    re-read starts at once. If the stream just goes quiet, the tick starts a request within
    `TERMINAL_POLL_INTERVAL`. Either ends within `REQUEST_ALLOWANCE`.
- The first counted failure lands at most 1 s + 30 s after the loss. The client reissues the
  failed re-read at once — a timeout already waited the allowance out, so it buys no backoff —
  and each reissue ends within its own `REQUEST_ALLOWANCE`. Four reissues after the first
  failure: 4 x 30 s.
- Worst case: 1 s + 5 x 30 s = 151 s, which is `TRANSPORT_STALL_EXIT_BOUND`.
- A killed or unreachable service fails fast instead: a connect-kind failure charges what it
  actually awaited — nothing for an instant refusal, the measured time for one that consumed
  it (a TLS handshake held before the close) — and buys budget-clipped exponential backoff
  (1 s doubling to 8 s), so a dead loopback ends at the fifth refusal, about 15 s after the
  loss, well inside the bound.
- A late body failure — a truncated or delayed body that is not a timeout — charges what it
  actually awaited, capped at the allowance, and buys no backoff. The
  `MAX_TRANSPORT_STALL_TIME` total is spent on every measured wait and every backoff, and a
  pre-attempt check stops any reissue whose full `REQUEST_ALLOWANCE` would cross it, so every
  mix of slow and fast failures stays inside the bound: five 29 s connect-kind failures end
  at 131 s spent, before the fifth attempt is started.
- An embedded service that dies under the product is detected at once: a connect refusal probes
  the child, and a proven exit ends the run immediately with the child's exit status. The child
  is the only proof that an embedded service is gone; a timeout alone never proves it.

Prompt admission is not idempotent and is never retried, and admission, replies and prompts are
never routed through the stall policy. The contract does not change that.

**Typical values, measured, not promised:**
- A server that goes silent after admission: exit at about 151 s, through the fifth timed-out
  re-read. The pre-stall-policy witnesses exited at about 31 s, through the first timed-out
  request; their killed-loopback numbers (about 17 ms on Linux, about 2.0 s on Windows)
  predate the backoff and now end at the fifth refusal.
- A permission poll that stalls once (4 s of server-side silence against the witness's 2 s
  allowance) is reissued and the run completes about 5 s end to end (the witness
  `a_run_whose_permission_poll_stalls_once_recovers_and_completes`, red at the pre-fix client).

## 2. Server loss while stdout is back-pressured

**The CLI does not put a time bound on a stdout write. The reader sets the pace.**
- A reader that stops draining suspends the run inside the write.
- Server loss is noticed when the write completes.
- When the reader drains, section 1 applies from that point.
- When the reader closes the pipe, the write fails and the CLI exits Output (74).

A harness that starts `grapt run` with its stdout on a pipe must read that pipe. The capacity of
an undrained pipe depends on how it was created, and the CLI can easily exceed it:
- Python `subprocess.PIPE` on Windows: 4,096 bytes.
- Linux pipes: 65,536 bytes.
- A single re-listed message page is the whole page, which is often several KiB.

## 3. Cancellation, at any time, including while stdout is back-pressured

**Ctrl-C and Ctrl-Break on Windows, and SIGINT and SIGTERM on Unix, stay effective.**
- **Normal case:** a run parked at an await stops at once. The Session is interrupted, the run
  receipt is written, and the CLI exits Cancelled (130). If the Session had already settled, it
  exits Success.
- **Blocked write:** if the run is blocked in a write the reader is not draining, it cannot hand
  its output back. In that case, within `CANCELLATION_HANDOFF` (1 s) of the signal:
  - the Session is interrupted, as above;
  - the CLI exits with the class code alone (130 once the interrupt settles), because its stdout
    cannot take a receipt;
  - it writes nothing more to stdout or stderr;
  - it does not wait for the blocked write.
- Once the handoff has passed, the interrupt and its settlement polls are durable re-reads under
  the section 1 stall policy: each reissue within `REQUEST_ALLOWANCE`, the path ending at the
  fifth consecutive transport failure within `TRANSPORT_STALL_EXIT_BOUND` of the service going
  silent.

**How this is kept true.**
- The run owns its output on its own task, and the signal is awaited on the executor's task.
- Every output write runs in place of its runtime worker (`block_in_place`). A blocked write
  therefore leaves timers, I/O and the cancellation requests served, even when the runtime has a
  single worker.
- The binary exits without joining runtime threads, so a thread left in such a write does not
  hold the exit.

**Before this campaign,** the signal was awaited on the task doing the write. A signal during a
blocked write was deferred until the reader drained. Measured on Linux: 45 s, until the harness
drained.

## Scope

- Out of scope and unchanged:
  - attach, which issues a single request;
  - `acp`, which writes through tokio's asynchronous stdout.
- Under `--embedded`, the product the CLI launched is owned by the executor's client, not by the
  run's task. A run left blocked in a write therefore does not keep it alive: the product ends when
  the CLI exits, as it does after a normal run.
- A separate observation, not part of this contract, is recorded in RESULTS.md: the event
  stream's 30 s total timeout also rebinds a healthy stream every 30 s.
