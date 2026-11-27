# NATIVE-TUI-R5-01 — attended comparative run

For the observer (example or the coordinator). Everything below is copy-paste; nothing here
asks you to improvise a path or invent an argument.

The charter's MW-COMPARATIVE clause wants the named interactions **observed**,
not merely recorded. My own runs are author evidence and are labelled as such;
this file exists so an observer can drive the same journey themselves and see
what it does, with the candidate and artefacts pinned so what they watched can
be named afterwards.

## What you are looking at

Two clients, one product, the same declared actions:

- the **native** client — this campaign's Rust TUI
- the **retained** client — the shipped TypeScript TUI, for comparison

They are driven through a real ConPTY behind a browser terminal, so every
keystroke and paste is a real one and the screens are real frames.

## Before you start

Nothing needs building, and nothing SHOULD be built: the bytes you observe are
pinned in an immutable bundle outside my author root, so what you watch cannot
drift while you watch it. `READINESS.json` beside this file names the bundle,
its every SHA256, and the commits they came from. If that file is absent the run
has not been prepared — ask me for it rather than building from source.

Every command below is **PowerShell**. Continuations are backticks.

## 1. Provision an isolated product

I provision this and leave it serving; the readiness path, endpoint and pid come
to you in the message with this runbook. The product runs with no window of its
own — only the harness browser is visible.

If you would rather do it yourself:

Everything below is inside the bundle. The PRODUCT binary is in it too, so no
command here reads my working tree, and none of them names another campaign's
preserved distribution — the previous runbook did both.

    $B = "<bundle root from READINESS.json>"
    $S = "C:/fixtures/native-tui-r1/service-attended"

    python $B/scripts/service.py prepare `
      --root $S `
      --inputs C:/fixtures/native-tui-r1/j04-service `
      --binary $B/grapt-server.exe

    python $B/scripts/service.py serve `
      --root $S `
      --binary $B/grapt-server.exe

A service root is prepared once and refuses reuse, so an observed run can never
describe a store some earlier run created. The provider is real, so the answers
you see are real external answers.

## 2. Drive the comparisons

SIX runs, each independent, so a blocked dimension cannot block the rest. `$B`
is the bundle root from `READINESS.json`; every path below is inside it, so none
of these commands read anything from my working tree.

    $B = "<bundle root from READINESS.json>"
    $S = "C:/fixtures/native-tui-r1/service-attended"

Selection, streaming, live execution state, and interruption through each
client's own binding:

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j04-comparative.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-comparative

PERMISSIONS. A tool asks for authority mid-run and the answer decides whether
it acts. Watch that the ask names the exact command, that the transcript shows
the tool row it belongs to, and — on the refusal run — that the named file is
NOT written. Approval first:

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j03-approval.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-approval

Then the refusal, which is the one that matters — an approval that works proves
less than a refusal that holds:

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j03-refusal.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-refusal

Each of those can be checked against the durable record afterwards. This reads
the store READ-ONLY and refuses corrupt or ambiguous evidence rather than
skipping it, so a green result is a real join and not an empty one:

    python $B/scripts/correlate.py --service-root $S `
      --out C:/fixtures/native-tui-r1/attended-refusal/durable-extract.json

Input, PASTE, Unicode and resize — this is where the multiline and non-ASCII
evidence lives. `j04-comparative` pastes only single-line ASCII, so it does not
substantiate the Unicode claim on its own. TWO scenarios, and the difference
matters: the first drives the REAL PRODUCT and is what closes the claim; the
second drives the contract double, which is the right place to push the
terminal edge deterministically but cannot say anything about the wire.

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j02-input-product.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --only native `
      --evidence C:/fixtures/native-tui-r1/attended-input-product

    python $B/scripts/journey.py `
      --scenario $B/scenarios/j02-input.json `
      --binary $B/grapt-tui.exe `
      --evidence C:/fixtures/native-tui-r1/attended-input

A 300-line answer, scrolled with PageUp/PageDown on both clients:

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j04-large-history.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-history

A delegated turn, added at r3. The model asks for a subagent, the ask appears as
an authority decision like any other tool, a CHILD Session runs its own turn, and
its answer comes back attributed to the call that asked for it. Answer the ask in
the client: `o` on the native side, Enter on the retained one.

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j04-subagent.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-subagent

A producer's question, added at r3. The runner posts a real form through the
product's own route, on the session the client has open — a form comes from
something that needs an answer, never from the client that shows it. Watch the
native client find it WITHOUT being prompted to, take a pasted answer into the
field, and settle it.

    python $B/scripts/compare.py `
      --scenario $B/scenarios/j04-forms.json `
      --native $B/grapt-tui.exe `
      --service-root $S `
      --ts-root C:/workspace/repo `
      --evidence C:/fixtures/native-tui-r1/attended-forms

Reconnect. The connection is CUT under both clients and each must recover. This
one drives itself: it stands a byte-for-byte relay in front of the product,
because the product admits only an ephemeral port and a restart would move the
endpoint rather than break the connection.

    python $B/scripts/reconnect-run.py `
      --service-root $S `
      --native $B/grapt-tui.exe `
      --ts-root C:/workspace/repo `
      --scenario $B/scenarios/j04-reconnect.json `
      --evidence C:/fixtures/native-tui-r1/attended-reconnect

The harness browser is visible throughout. On the comparisons the native side
runs first, then the retained side against the same product. `--only native` or
`--only retained` runs one side if you want to watch one without the other.

## 3. Stopping the product

I own that and will do it when you say you are finished, so nothing disappears
underneath you mid-observation. If you need it stopped yourself, stop it by the
pid in the readiness message — never by image name, because this machine runs
other people's products.

    Stop-Process -Id <pid> -Force

## What to watch for, and what each thing means

- **The opening screens.** The native client requires an explicit project,
  agent and model choice; the retained client enters its launch folder with the
  location's defaults already applied. That is a real difference in steps and
  discoverability. J01 requires the explicit selection, so the native behaviour
  is the required one — the comparison records what it costs rather than
  excusing it.
- **The paste** — in the `j02-input` run, not the comparison. A multiline paste
  must COMPOSE in the prompt, not submit its earlier lines. This was broken on
  Windows until `910bb36`, and the before/after is preserved in `j02-double`
  (defect) and `j02-at-910bb36` (fixed).
- **The non-ASCII text**, also `j02-input`. `— naïve café Ω ∑ 🙂 日本語` must
  arrive intact; `U+2014` and `U+2211` were being dropped before reaching the
  client at all. The comparison scenario pastes plain ASCII and says nothing
  about this either way.
- **The interrupt.** Each client is driven through ITS OWN configured binding —
  Ctrl+C for the native client, Escape for the retained one, whose Ctrl+C is its
  exit binding. An earlier revision of the scenario compared the keystroke
  rather than the action and drew a false conclusion from it.
- **The timings.** Every figure is end to end, measured outside both clients by
  a monotonic clock. It is the right quantity for a felt comparison and the
  wrong one for blaming any single layer. Two sequential live provider calls
  differ for reasons neither client controls, so please do not read a single
  reply-time pair as a client difference.

## Subagents — what you CAN see, and what does not exist to be seen

Two different things wear this word, and only one of them is observable.

- **The subagent MODEL** is a label on the session you are in: what a subagent
  would run as. It is on the header line, and it keeps three states apart —
  nothing read, the contract answering NULL (`subagent inherits`), and a real
  selection (`subagent <provider>/<model>`). Showing `-` for both unread and
  inherited would report "nobody said" where the product answered. Watch the
  header when you open a session.
- **A subagent's own SESSION** would appear in the picker marked
  `(subagent of <parent id>)`, so a subagent's private transcript cannot be
  mistaken for a conversation you started. **You will not see one on this run,
  and that is not a client defect.** Nothing in the product creates a child
  session: `with_parent` has no production caller, `session.forked` is never
  emitted, and there is no task or spawn tool. The client half is built and
  witnessed against the contract double; the producer does not exist.

## What this run does NOT cover

Named so the receipt cannot be read as broader than it is.

- FORMS are not exercised. Nothing raises one at the accepted base, and the
  routes depend on a reviewed API release from the backend lane which has not
  been consumed here.
- SUBAGENT CHILD LIFECYCLE — a child session starting and ending — is not
  exercised, because no producer creates one (see above). Lineage PRESENTATION
  is witnessed against the double; the real-product journey has no producer.
- THE MODEL PICKER is exercised only against a small catalog. The real
  configured catalog is 366 models and, until the reviewed release carrying
  `decode_page` is consumed, a response that large is refused at the client's
  codec. The picker's own windowing is fixed and witnessed, but an attended run
  will see nine models rather than the real inventory.
