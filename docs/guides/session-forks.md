# Session forks

`POST /api/session/{sessionID}/fork` creates a separate Session with fresh
message identities and the parent conversation. An empty body or `{}` forks
at the latest history point.

To branch from an earlier point, send `{"throughSeq": 42}` with an exact
parent Session event sequence. The cutoff is inclusive and must still be
visible after any rewind. A prefix containing an unfinished assistant
message or local tool call is refused. The child retains the current agent,
model and preset; later conversation and queued work are excluded.

The parent remains unchanged. Forking copies conversation, including recorded
tool outcomes, but grants no permissions and performs no copied tool calls.
It does not rewind or copy workspace files.

In the native TUI, `/fork` opens a fork of the current history. `/fork 1`
branches before the latest initial user prompt; `/fork 2` selects the one
before that, up to 200. The selected prompt opens as an editable draft in
the child. It runs only when you send it. Use Sessions to return to the parent.

API clients can send `{"beforeMessageID":"msg_..."}` instead of `throughSeq`
to branch before a surviving initial user prompt. Supplying both selectors
is invalid. Hidden prompts and inputs that steered an execution are refused.
