# Assistant response grouping

C04 exposed a replay error: one assistant response's two calls committed at
seq10/12, their results at seq13/15, and buffered assistant text at seq14.
Replaying commit order put text between matching results, and the native C04 request received an HTTP 400.
A minimal response with the same two calls and no interleaved text succeeded.

The sequence sort came from `67d34257` (prior-conversation reconstruction).
Chat's one-message-per-item projection came from `1a156c38` (provider transport).
This fix changes request assembly only. It introduces no event, schema, durable
write, dependency, or provider header.

Raw history rows keep their sequence order for compaction cuts and watermarks.
Emitted coding input instead follows the durable assistant response and content
ordinal, followed by every corresponding result in call order. Request-only
response boundaries preserve separate responses, including adjacent text-only
responses. They count toward the existing 512-item context limit and are never
emitted as vendor items or charged as token framing. The structural compaction
probe counts one marker per retained response alongside its content, so it
triggers before the request bound refuses that history. Existing byte limits apply.

Chat emits one assistant message with its text and ordered `tool_calls`, then
all tool-result messages. Anthropic emits one assistant content-block array,
then one user block array containing all results. Responses keeps content and
function-call items in their original ordinal order before all function outputs;
text before calls keeps that position. Signed replay retains its original bytes,
protocol, model and binding checks. Result-content and cache-marker policy stay
unchanged; cache marking runs after grouping.

Already-valid histories change request bytes too: separate assistant messages
for calls become one. The first request after upgrading can therefore miss its
old prefix cache once. Later requests rebuild the same prefix deterministically.

The tests use real durable writers and artifact roots for text before, between
and after calls; the delayed-close case reproduces C04's exact seq10–15 layout.
Reopening the store must yield identical request input. Serializer witnesses
cover all three formats, preserve response boundaries and text ordinals, and
assert byte-stable serialization. Missing, duplicate, unmatched and interleaved
results refuse before transport. Existing failure, withheld-output, retained
compaction and replay-binding tests remain part of the affected gate.

The approved 512-byte opt-in error-text feature is a separate slice; private diagnostic capture code is not part of this fix.

The one approved direct replay of a reconstructed bad segment returned a 400
with vendor text: “This model does not support assistant message prefill. The
conversation must end with a user message.” This is a captured response to the
minimal reconstruction, not the lost original C04 body. The interpretation that
OpenRouter translated the interleaved shape into a final assistant turn remains
an inference. Actual coding plans assert that their final item is user or tool content in
unit fixtures. Durable grouping witnesses include interrupted histories and
reopened stores; historical-reader tests can still inspect completed responses.
Wire witnesses assert the same invariant for Anthropic, Chat and Responses.
