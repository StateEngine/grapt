# StateEngine gateway preview

Run `cargo run -p grapt-tui --example gateway` to try the project, agent and
active-session screens with sample data. Type to filter the current page,
use arrows and Enter to choose, Escape to go back, F5 to reload and F8 to load
another page. Ctrl+C exits. Add `-- --disconnected` to exercise the path that
skips the gateway when StateEngine is unavailable.

This development example uses the proposed StateEngine selector read contract.
It makes no engine calls and grants no session authority. The normal native
TUI is unchanged. Connecting it requires the real selector API and the existing
engine binding operation. Project creation/decommission and saved-session
listing require further API operations; the preview exposes none of them.
