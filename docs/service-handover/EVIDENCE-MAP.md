# Service handover evidence map

R6-SERVICE-HANDOVER-01 r1, accepted base
`0aede982e9867e024f9aec237d5dcca2938f0ab3`. This map describes executable
witness coverage. Final run receipts and independent acceptance are separate;
the existence of a test or mutation row is not evidence that it passed.

## Contract and ownership

The explicit product `handover` operation consumes release rehearsal evidence,
observes retirement, acquires service election custody, reads the source afresh,
and opens the platform store. Release compares the actual open store's domain
and measures its file fingerprint. Product passes the actual authenticated probe
to the existing service transition and registration CAS. There is one store
owner and one election; no migration algorithm or service manager is added.

Ordinary startup also acquires that election before store effects. This changes
contention ordering:
a concurrent starter refuses before migration. Failed preparation releases the
election without replacing registration. Impossible read-only store admission
refuses before election creation; actual SQLite open remains after election. Normal binaries compile out
the default-off crash feature's selector and abrupt exits.

The predecessor's J01 completion claim was withdrawn. Expected fingerprints and fabricated probe
attribution were removed from the handover path. These corrections require the
new evidence below; predecessor happy-path receipts do not establish them.

## Journey coverage

| Journey | Positive and failure witnesses | Resurrection rows |
| --- | --- | --- |
| J01 | `handover_journey`: explicit verb and four-cycle live-product database journey; release `serving_attestation_measures_the_open_store_and_rejects_changed_domain` | SHV03, SHV06, SHV07, SHV10 |
| J02 | Live non-retiring/PID-reuse proxy remains alive; post-audit and retirement-window source writes refuse before migration; replaced/empty registration and initial/late wrong build refuse; concurrent ordinary starter is excluded; `validated_handover_excludes_reentry_until_its_custody_is_released` checks three attempts after validation | SHV01, SHV02, SHV04, SHV05, SHV08, SHV09, SHV14, SHV15, SHV16, SHV17 |
| J03 | Feature crash journey observes actual registration, schema and readiness before/after retirement and commit, then reopens or refuses; recovery cut preserves state; missing/corrupt checkpoints refuse; actual unattributed probe cannot elect; postactivation writes survive restart and disposition reports backup loss | SHV11, SHV12, SHV13; inherited MXT restore/disposition evidence below |
| J04 | `live_product_handover_preserves_a_product_created_database`: four real predecessor/successor cycles with distinct instances, unchanged canonical domain, child reaping and registration/readiness cleanup; `SHV_CYCLE` records each measurement | SHV10 plus J02/J03 custody and crash rows |
| J05 | [Operator recipe](OPERATOR-RECIPE.md), [source/input pins](PROVENANCE.json), governed profile and final receipts; the path is operated independently before landing/reproduction | Final evidence must contain named red and restored-green results, not anchor checks alone |

The wrong-PID witness registers an unrelated live process and proves handover
does not retire or replace it. It does not force the OS to recycle a chosen PID.
The custody witness enters the real validation function and checks its filesystem
election before store preparation/commit; ordinary process contention separately
checks the real CLI entry point. Health refusal exercises the real election
function with an unattributed probe, while the positive process journey uses the
authenticated network probe.

## Bounds and custody

Four cycles target repeated retirement, identity renewal, readiness removal and
store continuity without claiming production soak. Each predecessor serves for
two seconds; fixture storage is limited to 256 entries and 64 MiB, with symlinks
refused. Retained rehearsal checkpoints account for measured per-cycle growth;
they are recovery evidence, not leaked live resources. Each cycle requires child
exit and removal of registration/readiness. Tests remove their own disposable
roots after success. The operator script instead retains its new root and logs
for review, with bounded commands and child cleanup in `finally`.

Registration reads cap at 8512 bytes. Election attempts are capped at three.
The CLI retirement wait is 30 observations at 100 ms intervals. Hostile-write
fixtures wait at most 20 seconds for the durable completed-rehearsal checkpoint,
then allow 300 ms for entry into the retirement wait. A missing milestone fails
loudly; it does not trigger a write in an unrelated phase. Crash selectors form a closed set and exit
with fixed code 86 only in fixture builds. Recovery assertions inspect durable
state; the exit code alone does not establish the boundary's effects.

## Inherited proof and limits

Accepted migration `d1a89622` and its exact review/POSTLAND receipt hashes are in
`PROVENANCE.json`. Reuse covers migration/backup identity, restore refusal for
absent/corrupt backups and aliases, and truthful postactivation loss disposition.
The final affected release/service batteries also exercise those unchanged
contracts. This campaign does not introduce an automatic restore or rollback
operation. A failed restore leaves the activated database in place and refuses;
the documented recovery path restores only to a separate destination.

Inherited evidence does not prove caller freshness, election lifetime, actual
served-store attribution, or the new crash boundaries. SHV witnesses establish
those integration properties. No daily-driver write, paid submission, production
cutover, public artifact or engine retirement is claimed.
