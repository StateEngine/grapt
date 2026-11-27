# Launching a StateEngine seat

Use a configured Grapt service/profile. Configure its provider and
product agent before launch. StateEngine supplies the Session identity, explicit
MCP identity binding, verified packet pointer and issued Session capability.
Provisioning starts no service. Once bound, the running service can react to
actionable mail for that seat.

Importing a signed-in ChatGPT account with `grapt code --setup` retains its
complete sign-in state as well as the access credential and model catalogs.
Setup refuses unreadable state or disagreement between its account/access token
and the captured provider configuration. It writes only the new private profile;
the source profile and its database remain unchanged. Fresh sign-in still uses
the normal sign-in flow. Cross-machine account use needs its own operator grant.

Start the service explicitly. It does not need the launcher's config selector
in its startup environment:

```sh
grapt service start --service '<profile>' --product '<installation>/grapt-server'
```

The launcher supplies `GRAPT_CONFIG` or the pre-rename `SECODE_CONFIG` when it <!-- grapt-rename: keep -->
runs `session provision`; both are accepted, `GRAPT_CONFIG` wins when both are
set, and the `SECODE_CONFIG` spelling reports a one-line deprecation notice. <!-- grapt-rename: keep -->
Provisioning forwards the absolute path through the authenticated API and binds
it durably to that Session. No service restart is needed. On Windows use the
`.exe` binaries. Keep `grapt`, `grapt-tui` and `grapt-server` from the same
installation.

Keep the generated file owner-only and outside every repository and `.grapt`
directory. Its StateEngine MCP environment must name the bound
`STATEENGINE_PROJECT_ID`, `STATEENGINE_AGENT_ID` and `SECODE_SESSION_ID`.
Missing files, unsafe protection and mismatched identities refuse with a reason;
they never select another config silently. The selected file is checked again
when read, including after a service restart.

This selection wins for that Session's MCP configuration. Other Sessions do
not inherit it. Omitting the selector retains an existing Session binding;
sessions without a binding use the service-wide `GRAPT_CONFIG` fallback.
The legacy config selector is accepted on provision only when `GRAPT_CONFIG` is absent.
The issued capability remains process-private and must be supplied again after
a service restart.

The launcher's MCP config uses the native `mcp` shape:

```json
{"mcp":{"stateengine":{"type":"local","command":["<node>","<se.mjs>","mcp","<explicit binding arguments>"],"environment":{"<binding variable>":"<explicit value>"}}}}
```

Provide the full project/root/registry/owner binding in the command and
environment. MCP children use their declared environment; they do not inherit
the client's ambient identity. Replace the seat's config atomically before
provisioning a new launch. The service rereads the file and retires MCP clients
whose declaration changed. Give each prepared Session its own generated config;
do not overwrite one principal's selected file with another principal's binding.

The launcher creates a fresh `ses_…` identity, writes and reads back the packet,
and obtains its capability from StateEngine's existing Session issuer. Then run:

```sh
GRAPT_CONFIG='<absolute seat-config.json>' grapt session provision \
  --service '<profile>' --session '<session-id>' \
  --agent '<product-agent>' --model '<provider/model>' --location '<repository>' \
  --identity-entry '<identity JSON>' --packet-entry '<packet pointer JSON>'
```

Pass the issued capability as the command's single stdin line, never in argv or
an environment variable. Stdin has a 30 second wait and a 64 KiB limit. Identity
JSON is an explicit project binding:

```json
{"scope":"project","project_id":"<StateEngine project>","agent_id":"<StateEngine seat>","mail_transport_protocol":"remote-seat-mail-transport/v2"}
```

The packet pointer uses the launcher's readback values, with an absolute path
on the machine running the service:

```json
{"state":"written","path":"<absolute packet path>","bytes":6784,"sha256":"<64 lowercase hex digits>"}
```

Provisioning creates the exact Session, stores its packet and identity entries,
then binds the capability last. Exit zero means all four API writes succeeded.
The final write also validates and audits a selected config binding. A failed
replacement leaves the previous binding intact; a successful replacement
invalidates that Session's published MCP catalog.
The supplied Session identity must be fresh and at most 128 bytes. A failed
creation can reserve that identity, so obtain a fresh one before relaunching.
A missing service, failed write or mismatched created Session refuses launch.
Writes are not retried, and partially created Sessions are not deleted: repair
the service and obtain a fresh launch identity before retrying.

Attach the interactive or headless client to that Session:

```sh
grapt-tui --service '<profile>' --session '<session-id>' --prompt '<seat-runner prompt>'
grapt run --service '<profile>' --session '<session-id>' --stdin --jsonl
```

R displays the draft while the saved agent, model and reasoning effort load,
and submits once after attachment. Editing, pasting, opening a picker or
navigating cancels automatic sending. Text is literal and limited to 64 KiB.
A failed startup read waits for Ctrl+O; this retries attachment reads, then
allows an unchanged startup prompt to send. Enter submits a retained manual
draft after editing or navigation. An uncertain submission uses Alt+R to check
or retry the original input, keeping edits separate. Missing choices or a
pending permission or question keep the draft for explicit submission.

An exact-session launch keeps its saved reasoning effort; change it in the
model picker after attachment. `--reasoning-effort` selects an ordinary launch
and cannot be combined with `--session`.

A build with `grapt-product/loopback-provider-testkit` is fixture tooling:
`grapt-server --version` labels it `testkit/fixture`. Keep its renamed binary
outside release installations. A fixture launch verifies launcher, resume and
bell integration; it does not replace a launch witness using the release build.

Before provider execution, Grapt reads the packet once for that delivery,
checks its exact byte count and SHA-256, and sends its `packet_id` in the first
StateEngine resume call. The response must acknowledge that exact packet and
verify the bound principal. Missing, unreadable, malformed and unacknowledged
packets give distinct delivery failures; no incomplete context reaches the
provider. Verified bytes survive later context windows in bounded process
custody. Rebinding or restarting requires delivery again.

Reconnecting the same declared MCP server refreshes continuity with the previous
cursor and verified principal. It preserves the already verified packet and its
private-handle redactions. A failed required read supplies no partial provider
input; send a follow-up prompt after restoring the connection. A changed binding,
capability, packet delivery or server declaration requires fresh bootstrap.

New `session provision` identities require `remote-seat-mail-transport/v2`.
Retained identity entries without a protocol selector use the v1 compatibility
consumer; an explicit selector chooses v1 or v2, rather than inferring it from
an endpoint or message. Finding mail requires v2 negotiation.

The capability-bound mail consumer connects to the user-owned configuration's
`stateengine.native_mail_endpoint`, an explicit `http://127.0.0.1:<port>` URL.
A missing, unreadable or invalid endpoint refuses subscription. An explicitly
selected config cannot borrow another file's endpoint. StateEngine owns the
consumer-bound witness and launch timeout;
Grapt writes no separate arm receipt. A provisioned Session still needs the
StateEngine launcher's witness before the seat is considered armed.

The service keeps this subscription while the governed Session is idle. New
actionable mail starts one input listing the pending message IDs; a burst is
coalesced, and reconnecting does not repeat an admitted input. If the Session
is busy, the input waits for its execution boundary. Later or overflow mail
stays pending for the next input. Ordinary sessions without a StateEngine
project binding, and incognito sessions, do not start automatically.
An explicit stop keeps the input pending but disables automatic recovery of
that input, including after restart. A new prompt can resume it; a duplicate
mail advisory or reconnect alone cannot.

The service log records `native mail subscription opened` and
`native mail subscription dropped`, with the Session ID. Use these alongside
StateEngine's subscriber status when diagnosing a silent bell. Missing or
revoked capability custody requires an explicit rebind.

Temporary transport losses show the inbox as offline and retrying. After three
fast retries, the same Session keeps reconnecting in the background with a
staggered backoff capped at 60 seconds. Mail resumes when the door returns;
authentication and contract refusals stay unavailable until repaired.

The inbox accepts native Finding mail when negotiated on v2. A future unknown
mail kind skips that row and shows an inbox warning while valid mail continues.
The warning contains no message body and grants no mail action or ACK.
Malformed pages and foreign bindings still refuse the page.

## Retained legacy launch fixture

`seat_walk.py` exercises the retired `grapt-tui` client with synthetic peers;
its build names below describe that fixture, not the packaged R launch above.
The Linux fixture walk runs both the headless client and native TUI against a
real product service with synthetic StateEngine and model peers:

```sh
python3 scripts/stateengine-native/seat_walk.py \
  --bin-dir /path/to/fixture-binaries --evidence /path/to/new-run
```

The binary directory must contain `grapt`, `grapt-server`, `grapt-tui` and
`grapt-tui-driver`; build the server with `loopback-provider-testkit`. The walk
requires Bubblewrap and tmux. It puts all processes in private network and PID
namespaces, so its mailbox uses port 4877 without touching the live listener.
It checks required inbox reads, packet reuse, idle and busy mail delivery, and
compaction. It does not qualify a live StateEngine launch or a full seat-day.

For a qualified ordinary Linux package, exercise one continued producer task:

```sh
python3 scripts/stateengine-native/sustained_walk.py \
  --package /path/to/ordinary-linux-package \
  --source <package-source-commit> \
  --driver /path/to/grapt-tui-driver \
  --scratch /path/to/new-disposable-run
```

Run this through the normal heavy scheduler. It requires Bubblewrap, tmux and
OpenSSL. The fixture uses a disposable CA supplied only to its product process;
ordinary TLS verification stays enabled. The package must contain all five
release binaries and its manifest. The driver is a separate developer tool.

The task fixes signed addition, polls a real background terminal command, and
reads bounded evidence files until the default automatic compaction policy runs.
Another input queues during that summary. The same Session then survives a
disposable service restart and adds tested subtraction support. The fixture
checks committed compaction, exact admitted input identities, retained tool
outcomes and new background handles after restart. All profiles, credentials and
peers are synthetic; a passing result is Linux integration evidence, not a real
producer seat-day or Windows acceptance.

## Checking the R composer on Linux

`r_sustained_walk.py` exercises the same continued task through the real R
composer, using ordinary binaries built together from one source checkout.
It does not use a release package or change the default launcher. Build the
server without `loopback-provider-testkit`, then stage the four executables:

```sh
bash scripts/shared-cargo.sh build --locked \
  -p grapt-product --bin grapt-server -p grapt-cli --bin grapt \
  -p grapt-tui --bin grapt-tui \
  -p grapt-tui-driver --bin grapt-tui-driver
source scripts/build-cache.sh
stage=/path/to/ordinary-r-binaries
mkdir -p "$stage"
for binary in grapt grapt-server grapt-tui grapt-tui-driver; do
  install -m 755 "$CARGO_TARGET_DIR/debug/$binary" "$stage/$binary"
done
git rev-parse HEAD > "$stage/source.txt"
python3 scripts/stateengine-native/r_sustained_walk.py \
  --binaries "$stage" --source "$(git rev-parse HEAD)" \
  --scratch /path/to/new-disposable-r-run
```

Use a clean source checkout and run the build and journey through the normal
heavy scheduler. Bubblewrap, tmux and OpenSSL are required. The fixture keeps
network/PID namespaces, disposable TLS trust and profiles separate from live
services. All model responses and StateEngine peers are local fixtures.

The Linux journey checks one continued coding task across three automatic
compactions. R retains an unsent draft while background tool output and
compaction arrive; editing and sending that draft queues its exact text once.
Eight real shell/patch approval dialogs show context and receive **Once** through
R's `y` key. A disconnected approval leaves its patch unapplied until explicit
retry of the same request. Five input identities, their exact text, committed
permission replies, verified tool outcomes and tested file changes are checked.
The same R process stays open during a graceful disposable service restart.
It keeps the edited, unsent draft and reconnects through its original `--service`
profile after both endpoint and credential change. R verifies the replacement's
readiness and product identity before using it. Missing, malformed or mismatched
readiness keeps R offline with the draft intact; recovery retries use bounded
backoff. Once the fixture explicitly rebinds its synthetic StateEngine capability,
the edited draft is sent once into the same Session and completes further tested
work. The fixture loses that input's response, keeps the uncertain draft visible,
and uses **Alt+R** to check the original submission without admitting it twice.
Further automatic compactions retain both tested operations and their objective.

The same R process also survives a crash while a nested tool is waiting and a
Session question has an edited, unanswered draft. The question and composer
drafts survive; the explicit answer settles once after restart. `/reconcile`
records the lost tool outcome as unknown once, without repeating its effect.
The continued task then verifies the existing effect and both operations with
real tools. R exits cleanly with two Ctrl+C presses within 1.5 seconds while the
service is offline.

Reconnection reads saved history and permission state; it never resubmits earlier
input or reruns completed tools. An input whose reply was lost keeps its original
submission identity. Editing that draft does not cancel or replace the unresolved
submission; explicit retry still uses the original identity. Service discovery
cannot rebind a StateEngine capability: that remains the launcher's operation.

### Checking the same Session recovery on Windows

`r_windows_reliability.py` runs the same fault sequence in a native 160×45
ConPTY, using Node with `@lydell/node-pty` 1.2.0-beta.12 and
`@xterm/headless` 5.5.0 installed in the supplied dependency directory.
Stage matching Windows `grapt.exe`, `grapt-server.exe`, `grapt-tui.exe`
and their source commit in `source.txt`. This carrier explicitly requires
the labeled `grapt-cli/loopback-testkit` and
`grapt-product/loopback-provider-testkit` build features:

```powershell
python scripts/stateengine-native/r_windows_reliability.py `
  --binaries E:/temp/grapt-binaries --source <source-commit> `
  --scratch E:/temp/grapt-new-run --node-modules E:/path/to/node_modules
```

Use a new private `E:/temp/gamma-*` scratch directory for every run. Add
`--smoke` to check startup, literal input and clean exit before the full run;
that result does not qualify the recovery journey. The full run writes a
PASS/FAIL/NOT RUN table in `steps.md`, exact process, HTTP and ConPTY commands
in `commands.jsonl`, and terminal captures beside the disposable profile.
Keep these artifacts outside the source repository.

The Windows carrier substitutes ConPTY for tmux and native Python commands
for POSIX shell commands. Killing its owned Product process injects the crash.
All Session, input, approval, question, tool-effect and compaction assertions
remain shared with the Linux journey. The plain-HTTP loopback model needs no
system trust changes; it does not qualify TLS, an ordinary release binary or
a release package. Session creation uses the authenticated Product API and
does not bind a native seat or start a mail peer on the live engine's port.
Native mail, seat binding and the actual producer seat-day remain separate
row A acceptance work.

If a crash loses a nested tool's result, the Session may refuse new input and
ask you to run `/reconcile`. In R, this command records the lost runs as
**unknown** through the existing Session recovery operation. It does not rerun
them or claim they completed. The result stays open until **Esc**. Inspect their
effects before continuing. A refusal
keeps the command editable; retry it explicitly once the Session can recover.

This is bounded synthetic Linux integration evidence. A real producer seat-day,
provider cache share, Windows behavior, R performance and release packaging remain
separate acceptance work.

The R startup journey checks literal `--prompt` text, delayed saved choices,
editing/navigation cancellation, explicit read retry and an uncertain write with
no replay, using the same isolated ordinary binaries:

```sh
python3 scripts/stateengine-native/r_launch_walk.py \
  --binaries /path/to/ordinary-r-binaries --source <source-commit> \
  --scratch /path/to/new-disposable-launch-run
```

It uses local Product/MCP/model peers and a disposable HTTP fault proxy inside
private network/PID namespaces. It makes no paid calls or live-profile changes.

### Questions in R

R checks the attached Session for outstanding forms every two seconds. Questions
open in their own pane and keep the composer draft. F6 reopens the pane; Escape
closes it without answering. Approvals keep priority over questions.

Use Tab/Shift+Tab for fields, Ctrl+N/P for questions, and Page Up/Down for details,
including full option labels, values and bounds. Text fields accept multiline
editing and paste. Numeric fields accept finite JSON numbers; integer fields
require whole numbers. Invalid numeric text survives switching fields and blocks
submission, including on optional fields. Minimum/maximum and text length bounds
are checked before sending.

For single-choice and multiselect fields, use Up/Down (Home/End for the ends) to
move through offered options and Space or Enter to select. Movement alone does
not choose an answer. Multiselect toggles each option; item limits are checked
before sending. Labels are displayed separately from the actual submitted values.
Space explicitly chooses a boolean; Delete clears a flag or selection. Ctrl+S
answers the whole question and Ctrl+R declines it. Enter never submits a question. Conditional fields follow active earlier answers;
hiding a branch keeps its drafts for restoration and omits them from the answer.
For custom single/multiple choices, Ctrl+E opens a text draft and Enter explicitly
chooses it; Tab or Ctrl+E keeps unchosen text without selecting it. Selected custom
items appear beside offered options and Space removes them. Pattern checks run
when Product receives an explicit answer; a refusal keeps the draft editable.
The current MCP schema conversion still refuses conditional, custom and pattern
keywords rather than weakening them.
Required fields must be filled; an unfilled optional field is omitted.
R holds up to 16 question drafts, 32 fields per question, 16 KiB per string and
64 KiB per answer, with at most 128 options per selection field. Numeric tokens
are limited to 128 bytes; overflowing integer tokens and nonzero values that
underflow to zero are refused. Extra questions remain outstanding and become
reachable as others settle.

Typed defaults fill the initial draft once and remain editable; refreshes do not
replace edits or submit answers. Product checks supported email, URI, date and
date-time formats, including its accepted legacy date-time readings.
Unknown field semantics and external acknowledgements remain visibly unresolved.
R never guesses an answer for them. Questions with unattributed owners cannot be answered from a
Session pane. Use a frontend supporting those fields, or explicitly decline an
addressable question.

A failed or uncertain reply keeps the original decision and answer draft. Ctrl+O
reads that exact form's state without replaying a write. An explicit retry sends
only the original answer or decline; edits cannot replace an unconfirmed decision.
A missing outstanding-list row does not mean answered. R displays authoritative
answered/cancelled state, including an answer settled elsewhere. Answering a
suspended question settles the form; it does not restart lost work.
