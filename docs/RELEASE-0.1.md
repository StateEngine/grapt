# Grapt 0.1 — replace Codex for our own seats

Target 2026-11-12. Definition of done ratified on 2026-10-30. 0.1 ships when
every criterion below is **yes**. Nothing else gates the release. A criterion's status
is one word plus at most one sentence: no evidence recitals, paths, hashes or counts of
tests. Commits and landings are not progress; closing a criterion is.

| # | Criterion (binary) | How it is checked | Status |
| --- | --- | --- | --- |
| 1 | An Grapt seat works at least 8 hours of admitted, not hand-picked, producer work on one pinned row A build, with no manual relaunch | Seat-day export plus the launch log for that window | yes on 10-31; 12 hours on 58e8a145 with no relaunch |
| 2 | Provider cache read share is at least 95% over that window, including compactions and retries | `seat-day.mjs` export from provider-reported usage | yes; the 97.7% measured on 10-31 was accepted (11-01) |
| 3 | Matched tasks, same model, same day: the Grapt seat and a Codex seat (gpt-6.1-sol high) each take three comparable real tasks. The Grapt seat finishes all three with a reviewer ACCEPT; its first-review ACCEPT rate and tokens per accepted task are within 20% of the Codex seat's; its cache share is at least the Codex seat's | Both seats' exports and independent review verdicts | no; Grapt led on first-review ACCEPTs (3/3 vs 2/3) but its cache share was 97.7% vs the Codex seat's 98.7% |
| 4 | No lost user input, no turn stalled over 10 minutes without a visible cause, no crash, and at least one automatic compaction survived during the seat-day | History census and server log for the window | yes on 10-31 |
| 5 | Infrastructure tool failures are at most 3% of the Grapt seat's tool calls, and every failure names its cause | Tool-call census over the seat-day | no; 0.29% infrastructure failures, but one terminal failure has no named cause |
| 6 | One scripted Windows smoke runs every Codex tool family (shell with PTY and background, apply_patch, web search and fetch, plan, image, MCP stdio and remote, AGENTS.md and skills) on each row A build | The smoke script, green on the pinned build | yes on 19edbe07; rerun per row A build |
| 7 | `se seat launch` starts an Grapt seat, both TUI and headless, that answers through StateEngine MCP; idle mail wakes it; a takeover leaves exactly one active session | Launch receipt plus `GET /api/session/active` | partial; Windows launch, mail wake and single session passed 10-30, headless open |
| 8 | The seat survives a StateEngine restart without a relaunch | Restart during a live session; the next turn completes | yes on 10-31; six restarts with no relaunch and no MCP failures after the stale-client fix |
| 9 | A one-hour Claude API-key session reaches at least 95% cache read share | Session export | yes, 98.44% over 115 calls on claude-sonnet-5-5 (10-30) |
| 10 | The Grapt seat's transcript is accepted in hands-on use: prose replies, described tool rows, live status | Hands-on review | yes; the redesigned TUI was accepted on row A 92fa85f9 (11-01) |
| 11 | Windows Terminal: composer echo p95 at most 16 ms while streaming at 200 tok/s, and idle CPU about zero | Existing echo/idle harness | yes |
| 12 | The release package builds from `main` with committed pins, and installing it puts `grapt` and `se` on PATH | Package build plus a fresh install | yes on 58e8a145 (standard user, 10-31) |

Seat-day: ran 2026-10-31 06:58Z to 19:00Z on row A 58e8a145 (moved up from 11-05).
From 11-01 there are no scheduled seat-days: the Grapt seat stays on the Grapt TUI
doing regular work, at least one seat always runs on it, and criteria 3 and 5 are
rechecked on windows of that work.

## Deferred past 0.1 (tracked, not gating)

- The competitor-derived native TUI ceilings (startup, CPU, wakes, memory, history),
  conhost as a release host, and ETW wake measurement. Criterion 11 is the 0.1 bar.
- Windows HKCU Path upgrade qualification on a disposable account or VM.
- macOS seat-day acceptance; macOS CI stays green.
- Question forms beyond the basics, approval-context classifier work, Windows
  acceptance of failure/recovery feedback.
- Desktop, ACP, Campaign Mode and cutover.

