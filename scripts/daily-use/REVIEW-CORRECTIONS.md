# DAILY-USE-BACKEND-01 r2 review corrections

Review source: REVIEW-EPSILON.md
SHA256 a571939e1b3714f67477c9001f50f3da1e2d599314927e4875d98a3112973c1e.
This is a correction checkpoint, not acceptance or landing evidence.

## Corrections

- B2: the existing MCP transport follows opaque tools/list cursors under one
  catalog deadline. The pure protocol owner caps total tools, aggregate result
  bytes, pages (including empty pages), and cursor bytes. Cycles, malformed pages,
  duplicate tools, and incomplete catalogs refuse startup without publishing a
  partial catalog. The previous single-page call omitted the frozen client's
  paginate behavior (`packages/core/src/mcp/client.ts`).
- B3: the retained header selects the last usable total for its gauge and names
  it as the last measurement when newer attempts lack totals. Its denominator
  belongs to that measured message's model. Latest-message detail remains separate:
  missing fields stay unavailable, and reported zero stays zero.
- B4: `replay_mutations.json` carries portable definitions, exact-one source edits,
  commands and assertion-failure patterns. `replay_mutations.py` validates all
  selected anchors before mutation and restores original bytes even on refusal.
  N10's PowerShell driver validates command names from its profile, including
  workspace and correction commands, before running the storage guard or job.

## Scope judgments

- N1: global forms are directory scoped in this composition. The actual location
  resolver explicitly rejects every workspace-qualified request with "requested
  workspace has no registered location resolver". The reviewer scenario is not
  currently an admitted workspace workflow; no workspace isolation or supported
  workspace-session global-form claim is made. Adding a resolver or silently
  discarding workspace coordinates would broaden the contract. Deferred.
- N2: the legacy latency table refuses sessions with multiple executions instead
  of combining first events from unrelated attempts. The J04 bounded extractor
  remains the separately attributed measurement path.
- N3: verified owner is `packages/app/src/components/session-switcher.tsx`.
  Failure of the subagent list no longer discards a successful model response;
  null still means agent default. No backend subagent behavior added.
- N4: removed the obsolete regex-lite dependency explanation.
- N5: removed the unsupported assertion that the earlier receipt named the
  existing Debug-rendered StepFailed presentation debt.
- N6: the comparison summary filters absent failures instead of printing nulls.
- N7: independent tool-result bound and newline accounting are deferred. The
  current transport still bounds the complete frame and the existing parser
  imposes its conservative schema-sized text allowance; changing that allowance
  is separate from catalog pagination. No claim of an independent result bound.
- N8: the deterministic pattern budget remains deliberate CPU admission; large
  otherwise valid inputs may return Exhausted. No guarantee that every input
  below the answer byte cap can be evaluated within that budget. Deferred.
- N9: cache-pressure UX consistency is deferred. Existing execution eviction
  invalidates outstanding refresh identity; selection admission refuses excess
  entries. Both are bounded but have different recovery behavior. No consistency
  claim is made and this correction does not redesign either cache.
- P1/P2/P3 remain pre-existing, explicitly outside this correction campaign.

## Replay and evidence attribution

Run `python scripts/daily-use/replay_mutations.py --root CHECKOUT --tree rust
--check` first. Use `--tree retained` for the retained checkout and `--bun PATH`
when Bun's executable is not on PATH. Omit `--check` and supply a fresh `--output`
directory to run red then restored green; repeat `--id` to select rows. Heavy Rust
runs use the governed `daily-use-review-mutations` profile command. Run only in an
exclusively owned checkout, with no concurrent test, build, or mutation process.

The existing platform DUB01–DUB10, form DUF01–DUF15, producer CB01–CB03 and
global-form/provider/latency/write-gate script rows retain their committed drivers.
The replay manifest adds the previously ad-hoc OU, GUI, UI, MS rows and new PG/CG
correction rows. The final closure bundle must cite this census and exact receipts.

Historical failed runs are not overwritten: observed-ui-mutations-01 OU05
survived; observed-ui-mutations-02 contains the corrected schema-validator
mutation. The original zero-cache routes remain true of those routes; the
separately attributed seven-attempt OpenRouter cache-hit witness closes J03 reuse.
The b5bd0ee full workspace green was recorded separately. Since B2 changes production
Rust, a new full gate at the corrected candidate is required.
