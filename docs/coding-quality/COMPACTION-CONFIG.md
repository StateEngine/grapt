# `compaction` config (CODING-QUALITY-01, ruling 2026-10-07)

SE reads the oracle's `compaction` key from opencode.jsonc
(`core/src/config/compaction.ts`): `{ auto?: boolean, buffer?: non-negative int,
keep?: { tokens?: non-negative int } }`, folded over the ordered config documents with
a later document winning per key (`session/compaction.ts` `settings`). The service root's
own `opencode.jsonc` (the file that also holds provider and catalog settings) is the
lowest-precedence base; the Session's sources (user config, its project's ancestors,
`OPENCODE_CONFIG`) fold over it, so a Session working outside the service root still
follows the root's policy. A value of the
wrong shape refuses the chain by name. Only an absent setting takes the default: a config
source that cannot be read (malformed, oversized, not a regular file) refuses the chain,
as the catalogs refuse it, rather than silently replacing a configured policy.

The root-level `model_auto_compact_token_limit` optionally requests compaction
at an earlier provider-reported context count, following the provider's token-limit
control. For example, `"model_auto_compact_token_limit": 600000` compacts at
600000 input-plus-output tokens. It must be a positive integer; later config
documents override it. It cannot delay the universal 90% ceiling, and the earlier
of the two limits applies even when the projected-input guard is disabled.
Missing provider usage stays unknown; local estimates retain their existing
independent bounds.

For OpenAI models on the ChatGPT/OAuth route, the standard declared window is
the default. `gpt-6.1-sol` defaults to 272000 tokens; its 872000-token maximum
is used only with an explicit `"model_context_window": 872000` in the profile's
`grapt.jsonc`. The override must be a positive integer and is capped at the
model's maximum before the existing 95% allowance. Other provider routes retain
their existing window policy.

Fresh backend discovery takes precedence over the bundled defaults. First-run
ChatGPT discovery records its fetch time; on publication, snapshots older than
five minutes or without that marker use the bundled standard for known models.
Unknown models need a declared standard window; a maximum alone does not state
a safe default. This does not make a network request or change an inventory
already in use by a running service.

For Sol's default, Grapt has 258400 usable tokens and a 232560-token universal
usage ceiling. Codex has the same 258400 usable tokens but derives its 244800
compaction trigger from the raw standard window. Grapt keeps its current
90%-of-usable ceiling and projected-input guard, which may compact earlier.
Compared with the old saved 500000-token listing, long sessions compact more
often. That can add summary calls and cache misses around compaction boundaries;
normal stable-prefix caching remains unchanged.

Use `/compact Preserve the API decision and unresolved failures` to add
instructions for one summary. Both TUIs and ACP accept the command; the HTTP
route accepts `{"instructions":"Preserve the API decision"}`. Instructions
must contain text and fit within 16 KiB. The admitted request survives restart,
runs before the next message, and keeps its guidance through a summary retry.
Guidance does not change the standing summary format or later coding prompts.
A second pending request with different instructions returns a conflict.

- `auto: false` turns off the additional projected-input threshold. It cannot
  disable the 90% provider-usage ceiling, a structural bound or `/compact`.
  Repetition or a blocked recovery condition refuses further over-threshold work
  through the existing compactor instead of sending another coding call.
- With automatic compaction enabled, every provider compacts when its last call's
  reported input plus output reaches 90% of the selected model's context budget,
  before the next coding call. A reported total is used where available; reasoning
  already included in output is not counted twice. Missing output stays unknown,
  with reported input a lower bound. Local estimates remain additional admission
  guards for new tool results, large inputs and calls without reported usage.
- `buffer` replaces the 4 096-token headroom in the additional projected-input
  guard: `input + output reserve + buffer < budget`. It cannot delay the 90%
  usage trigger. The budget comes from the account inventory's effective model
  context window. Output reserves remain provider-specific. The rebuilt request
  must fit independently; without new history, SE avoids summarizing the same
  summary repeatedly.
- Both the projected-input guard and the recorded coding-request count are
  judged in tokens at the target's own observed confirmed-per-estimated ratio.
  That is the rate the provider charged for the same bytes. The conservative
  double-ratio margin bounds only content SE packs into a budget it chooses to
  fill (a summary payload, a working set, a rebuilt request); it never stands
  in as a request's size. Judging at the margin compacted sessions at about
  half their real usage and refused a summary that fit. An under-count reaches
  the provider's own overflow classification, which the Step's single rebuild
  recovers before provider I/O continues. On an OpenAI-compatible route that
  classification is not restricted to OpenAI's own code: the servers this
  client meets there answer a too-long request with their own typed error or
  with prose that names the context window, and a bounded set of those words
  and phrases classifies as overflow. A refusal that names only an output
  parameter, a truncation policy or unrecognized text stays unclassified, so a
  healthy session is never compacted on a guess.
- `server_side` is accepted for config compatibility and reported as ignored.
  OpenAI Responses and ChatGPT/Codex always try provider compaction first; other
  protocols use SE summary. OpenAI compaction sends a fitting coding request with
a trailing `compaction_trigger`, matching the current Responses V2 path.
  If a new tool result makes that request too large, SE can use the last sent
  prefix while retaining the unseen tail exactly. The opaque item is committed
  as a `ProviderBound` summary and redeemed only by its producing target.
  An unsupported request, transient failure or unusable item falls back to SE
  summary in the same Step. Authentication, credit and deterministic context
  failures stop before that fallback. Existing attempt bounds still apply.
  TUI markers and logs distinguish provider compaction from SE summary, and the
  log records the provider fallback reason.
- `keep` is ACCEPTED AND IGNORED (named divergence). In the oracle `keep.tokens` sizes
  the one tail that is both retained and published as `recent`. In SE it could only
  size the published `recent`, which history replay recomputes per compaction from a
  constant, so honouring it needs the value recorded per compaction first (backlog,
  Tier 1).
- SE's own summary request is CACHE-WARM (named divergence). The
  oracle sends its summary as one fresh user message with no tools
  (`session/compaction.ts`: `messages: [Message.user(buildPrompt(...))], tools: []`), so
  nothing in it is cached. Codex instead re-sends the conversation and appends its
  summary task, and SE now does the same. The first summary call is the coding request
  that asked to compact, with its instructions, tools and input unchanged, and one last
  user message: the template and the task restated. OpenAI-family routes (Responses,
  ChatGPT/Codex, OpenAI-compatible) send `tool_choice: none`. Anthropic keeps the coding
  request's `tool_choice`, because its documentation lists a changed
  `tool_choice` among the changes that invalidate cached message blocks. There a tool
  call in the summary response is refused before any effect and counts as unusable.
  Measured reason: in three matched C04 Luna pairs on 2936f1d6, the client arm's
  three summary calls were 0% cached, about 123k of its ~313k fresh input. The provider
  arm's summary calls (the same request plus `compaction_trigger`) cached all but about
  5k each, which is nearly the whole 35% gap between the arms.
  The warm request is judged at its own size (template included); where it does not fit,
  the cold request (the history quoted as data) is sent instead. The objective guard's one
  retry is always cold, because quoting the history as data is the framing that keeps a
  summarizer from answering it.
  MEASUREMENT NOTE: the Anthropic cache holding across the warm summary call is
  documented but NOT yet measured. A re-run must record the summary call's
  `cacheReadInputTokens` on an Anthropic route beside the coding call before it.

Witnesses: unit fold tests (`compaction_config_tests.rs`: defaults, later document
wins, keep accepted and ignored, malformed values refused by name) and headless
journeys (`dogfood_headless/compaction_config.rs`: default does not compact; a buffer
moves the threshold; a summary still above the buffer commits and the execution
continues without a second compaction; `auto: false`
turns it off; `keep` loads and changes nothing).

Automatic compaction recovery follows the failure cause. Context overflow waits for a
model/context-budget change or rewind; authentication waits for changed account credentials;
credit/quota waits for a successful model response; a transient failure may retry on the
next user input. A summary call the Step's no-progress watchdog cancelled is transient:
its provider class travels in the attempt's terminal reason, and a stall says nothing
against the compaction. `/compact` remains an explicit retry for every cause. While automatic
compaction is paused, ordinary work continues below the reported-usage ceiling
when it fits the actual context window. At the ceiling, the compactor reports the
blocked recovery condition before another coding request is sent. All summary calls,
including the existing fallback, remain within the owning Step's physical-attempt budget.

Suppression is process-memory state for at most 256 blocked Sessions. A restart resets it;
at capacity a new automatic compaction refuses rather than evicting another Session's
block. Manual retry remains available.

After a client-written compaction, SE keeps the newest whole original user
messages within an approximate 4096-token budget, then presents them in their
original order before the unchanged summary, up to the 1280-message structural
bound. Selection reads the newest inputs regardless of Session lifetime. This retention
estimate rounds UTF-8 bytes up at four bytes per token per message; it is not
provider-reported usage. Images keep their original content and add their
existing image-token estimate. Full-request admission remains independently
bounded. A message that does not fit is omitted whole, along with earlier
messages; the summary carries that older history.

The retained messages are historical. Later corrections and the current input
take precedence, and completed or cancelled work stays completed or cancelled.
Recent exact tool exchanges remain paired; older settled calls appear only in
a factual ledger. Restart and resume use the committed retention policy, so they choose the same
original messages. Older descriptors keep their previous replay policy. Forks
and children inheriting history rebuild the full durable conversation with
fresh identities, preserving the original text and completed exchanges; they
do not copy the parent's compaction descriptor or promise its cache prefix.

Retaining an identical seed can preserve its cached prefix when the provider
still holds it. If that seed no longer fits, or the provider cannot reuse it,
continuation still works; cache read share may fall to the stable instruction
and tool prefix. Provider usage is the measurement of any cache benefit.
