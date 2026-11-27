# `/compact`: user-requested compaction (CODING-QUALITY-01)

Decided 2026-10-07. A benchmark found that Grapt has no manual
compaction: over ACP `/compact` reaches the model as plain text. The other agent
harnesses each run a native compactor on `/compact` and report the boundary.

## Contract (oracle parity)

The frozen oracle owns the HTTP shape (`packages/protocol/src/groups/session.ts`,
`session.compact`):

- `POST /api/session/:sessionID/compact`, body `{id?: MessageID}`, returns
  `{data: SessionPending.Compaction}` (`{...Admitted, type: "compaction"}`);
  errors 409 Conflict and 404.
- Admission appends `session.compaction.admitted {inputID, sessionID}`. At most one
  compaction is pending per Session: a second request returns the pending one, and a
  replay of the same `id` is idempotent (`SessionPending.admitCompaction`).
- The runner runs the pending compaction at its next drain
  (`SessionRunner.runPendingCompaction`) through the manual compactor, which commits
  `session.compaction.ended` with reason `manual`; a failure publishes
  `session.compaction.failed` for that input.

## Existing Rust pieces

- `grapt-event` registers `session.compaction.admitted.1` (manifest only; no writer).
- `session_pending.input_type` already distinguishes `user` and `synthetic`.
- `CompactionTrigger::UserRequested` exists and maps to reason `manual`; nothing
  produces it. `ProductCompactor` skips the no-progress repetition guard for it.
- Compaction runs only inside a Step (`compact_and_reload` with a
  `CompactionContext`), on the chain the drain opened for a promoted user input.

## Slices

1. **Admission (Tier 1: durable write).** `Inbox::admit_compaction` appends
   `session.compaction.admitted` and projects a `compaction` pending row, one per
   Session, idempotent on its id. Witnesses: admit/replay/second-request; the
   projector refusing a second admission and rolling its append back (append and
   projection share one transaction, so there is no state between them to crash
   in); and a mutant that admits two. The identity probe reads at most two rows
   and only the bounded Session identity; duplicated or oversized history is
   typed corruption.
2. **First Step of the next input (ruling, option iii).** The drain design
   below the line was withdrawn: the summary call is custody-bound to an owning
   Step's terminal (`ProviderCompactionCall::run` refuses "summary call has no
   matching owning Step"), and a compaction at idle has none. So `/compact`
   only marks the compaction pending, and it runs in the next input's first Step:
   - A pending compaction never holds input. The idle and steer queries promote
     inputs only (`input_type != 'compaction'`); the pending row stays.
   - `ProductPlan::plan` refuses with `CompactionTrigger::UserRequested` while
     a request is pending, but only for the first attempt of the chain's first
     Step (`step == initial_step && ordinal == 1`): never mid-chain, so a request
     admitted during a running chain waits for the next input. It is checked
     before the structural and threshold refusals, so at a structural bound the
     requested compaction is the one that runs and settles. The Step then runs
     the existing `compact_and_reload` path with the product compactor; with no
     planned request yet there is no rebuilt-fit snapshot, and the next
     ordinary admission checks the fit.
   - Identity: a compaction id keeps one durable meaning. `find_existing`, the
     shared owner for user, synthetic and compaction admissions, also finds a
     settled compaction by its own admission event, so its id is refused as a
     compaction in another Session and as an input in any Session, and a replay
     in its own Session answers `CompactionSettled`.
   - Success: the descriptor write (`session.compaction.ended.2`, trigger
     `userRequested`) also runs the inbox projector, settling the request in the
     same transaction.
   - Failure or no progress: `Inbox::fail_compaction` appends
     `session.compaction.failed.1` (reason `manual`, error `compaction.failed`,
     the request's `inputID`) and the Step continues on the uncompacted context;
     the durable failure advances the generation the Step reloads.
   - Interrupt: the same with error `aborted` ("Compaction cancelled"); the Step
     ends interrupted.
   - Process loss: the request stays pending and runs again in the next input's
     first Step.
   - Oracle divergences, named: the oracle runs a pending compaction at its next
     drain without waiting for input, and a failed compaction fails that drain.
     Here it waits for the next message, and its failure never fails the user's
     message. Clients must say so: "Compaction will run before your next
     message."
   - Witnesses (real drain, headless): runs and settles; fails, settles and the
     message still runs; interrupted settles `aborted`; cut by process loss,
     runs again and settles; at a structural bound (129 promoted inputs) runs
     once and settles with no automatic compaction beside it. Planner: requested
     only before the chain's first attempt. Inbox: an earlier compaction is never
     promoted as a queued input; a failure settles once with the oracle error; a
     settled id is never reused across Sessions or input types.
   - Admission atomicity is structural, not a separately run crash: the
     decision reads, the append and its projection share one governed
     transaction. Its counterexample is projector-refusal rollback (a second
     admission written past the inbox is refused and its append rolled back).
     Durability of a committed admission across restart is what the headless
     journeys show.
   - Backlog (later Tier 1): a compaction-owned receipt terminal, so a
     compaction can run at idle without an owning Step, as the oracle does.

   *Withdrawn drain design, kept for the record:*

   **Drain (withdrawn).** The coordinator's drain loop has one place where the Session would
   otherwise go idle (no steer promoted, no continuation owed). Slice 2 adds a
   `SessionApplication::compact_pending` call there, before a queued input is
   offered, and loops when it ran. A chain in flight is never cut: while a
   compaction is pending no steer promotes, the chain runs to its end, then the
   compaction runs, then held inputs promote. (The oracle also runs a pending
   compaction after each Step mid-chain; deferring it to the chain's end is the
   one deliberate difference.)
   - Runtime: `LocationRuntime::compact` builds the product compactor without a
     promoted prompt and runs it with `CompactionTrigger::UserRequested` under the
     drain's cancellation. The Session reads as active for its whole run.
   - Settlement: the descriptor write (`session.compaction.ended.2`) runs the
     inbox projector, which deletes the pending row; a failure or interrupt
     appends `session.compaction.failed.1` (reason `manual`, the request's
     `inputID`, `aborted` for an interrupt), which settles it the same way.
   - Slice 1 must not land without this: a pending compaction holds every input.
3. **API.** The route and `SessionPending.Compaction` DTO, 409 when the Session
   cannot accept it.
4. **Clients.** ACP advertises `compact`, and `/compact` posts the route and
   answers at once "Compaction will run before your next message."; the boundary
   is then reported from `session.compaction.ended` ("compacted, N tokens to M")
   or `session.compaction.failed`. TUI the same through its command palette.

Each slice lands on its own gate. Slice 1 takes Tier 1 treatment (durable
admission); 2 to 4 are Tier 2. Slices 1 and 2 go to review and the train together.
