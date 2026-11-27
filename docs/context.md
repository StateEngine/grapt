# Inspect instruction context

Use `/context` in the R TUI to inspect the selected session's **last-captured**
instruction state. The command is read-only: it does not refresh instruction
files, submit a model prompt, or create an instruction epoch.

The view shows instruction-key names and the captured `core/instructions` text,
including the instruction-file headings and contents that the session loaded.
Values of session instruction entries, including StateEngine packets, are not
shown. Before the first capture, it explicitly says instructions have not yet
been captured. If a file changes afterward, `/context` continues to show the
capture rather than claiming those newer bytes were sent to the model.

The same inspection is available through authenticated
`GET /api/session/{sessionID}/instructions/inspect`. It returns `captured`,
`ambient`, and `keys`. A missing session, corrupt captured history, or a failed
read remains an error, not an empty context. On TUI failure, the draft is kept.
