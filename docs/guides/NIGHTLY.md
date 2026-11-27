# Nightly gates and automatic reverts

Install the accepted controller from the Linux workstation:

```sh
control="$HOME/.local/lib/secode-release-snapshots/$(git rev-parse HEAD)"
python3 scripts/build/install_schedules.py \
  --repository /home/example/grapt/worktrees/nightly --agent-id YOUR_AGENT_ID \
  --control-directory "$control"
```

Choose a fresh controller version; installation never replaces an existing
version. Existing clones must have their nightly ownership marker. A fresh
installation can seed `--last-green` with a verified full-green ancestor.
The supervisor stays outside the compute queue; each full gate queues separately
and releases its lease before publication enters the normal landing helper.

To inspect a regression without landing or sending messages:

```sh
python3 "$control/nightly.py" --repository /home/example/grapt/worktrees/nightly --dry-run
```

The nightly only considers commits after its last green nightly. It repeats the
failure, bisects first-parent history, verifies the culprit's parent is green,
and tests a new revert on the current candidate. Missing baselines, changed
failures, conflicts, infrastructure failures and red parents stop publication.

A tested revert lands through `scripts/land.ps1` with its exact full-gate result
and attributed base. The helper refuses another canonical landing lease or an
advanced `main`; it never rewrites history. Later unrelated changes remain.
The author and coordinator receive the red test and revert result through
StateEngine. An unknown author prevents publication. Delivery failures after
landing remain visible in `~/.local/state/grapt/nightly/last-result` and
`pending-notifications.json`; the result still identifies the landed revert.
