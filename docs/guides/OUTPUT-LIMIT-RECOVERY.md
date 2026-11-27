# Output-limit recovery

When a model reaches its output token limit, Grapt retains the partial answer
and automatically asks it to continue. The continuation uses the same selected
model, ordinary Step driver, permissions and provider accounting.

Recovery has no Step allowance of its own: a truncated response is followed by
an ordinary Step, and the turn continues, through tool calls and further
truncations, until the model finishes or the user interrupts. Each Step still
permits at most five physical provider attempts, counting retries and summaries.
Opt-in Step caps (`--max-steps`, an agent's `steps`) count recovery Steps like
any other.

If a continuation fails, the execution reports failure and keeps the partial
output. Send another input to continue explicitly. An interrupt stops recovery
through the normal session cancellation path.

The continuation cue appears only in the provider request; it does not create a
human message in saved history. A newer steer supersedes that cue. Sources that
cannot retain partial answers continue to refuse truncation.
