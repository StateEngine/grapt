# Session preset at launch

Start with `grapt code --inspect`, `--author` or `--maintainer`.
When several flags are present, the strictest wins.

The selection applies once when you explicitly open a new or resumed session.
It persists on that session. Reconnects refresh its display without applying it
again. The header shows the selected preset (or `default`) and the server's
effective preset, including project and ancestor limits.

Wait for an explicit launch selection before sending the first prompt. A slow
or failed selection keeps your draft. Press Enter to send it after settlement;
reopen the session to retry a failed selection. While an execution is running,
the display says “applies from the next turn”: that execution keeps its captured
policy, and a later execution uses the committed selection.

ACP preset display remains deferred in BACKLOG.md.

The native Windows witness runs the launch parser and ordinary TUI controller
against a fresh product process. A loopback relay delays both preset response
heads by two seconds while the key loop continues. It checks new attachment,
fresh resume, an explicit resumed override, default display and persisted
selection. Stage its dogfood test executable, product executable, relay.py,
ready.py and native_runner.py together, then run the staged witness on Windows.
