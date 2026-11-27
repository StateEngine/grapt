# Hosted CI

Pull requests to `main` and pushes to `main` or `batch/` branches run `CI`. The workflow exposes one
aggregate status named `check`; repository rules must be configured separately.
A failure, cancellation or unexplained skip fails that status. Each platform
verifies its archive, shards and doctests before the root aggregate can pass.
Concurrent updates to one PR cancel its older run. Platform archives and test shards do not cancel
one another after a failure.

Once the repository is public, `Hosted gate` runs on GitHub-hosted runners for
every pull request and every push to `main`, and `CI`'s self-hosted jobs skip. Its
own aggregate, `hosted-check`, passes only when every hosted job succeeded. On a
private repository it runs only for pushed `hosted/**` branches. When the
repository goes public, switch the ruleset's required status from `check` to
`hosted-check`.

Self-hosted jobs use disjoint custom runner labels: `grapt-precheck` for pull
requests and `grapt-landing` for batches, main, dispatches and nightly runs.
Assign exactly one runner to each label, with no runner carrying both. A missing
landing runner leaves its jobs queued; the workflow has no fallback to the PR
runner. The legacy general label may stay on the precheck runner for older refs.
Changing the YAML does not register or relabel runners. The designated
infrastructure operator applies live runner changes after the workflow PR lands;
keep labels needed by older refs during the transition.

Native Windows runs on PRs that touch Windows or platform-sensitive code,
terminal layers, credentials, permissions, packaging, dependencies or CI/build
inputs. Unknown paths, new code, removals and renames retain native Windows.
Prose and explicitly listed portable data/codec leaves can omit native execution.
Both revisions of each code leaf must remain free of platform effects and
Windows guards. The detector prints the decision and reason.
An omitted native leg instead requires Windows-target Clippy with warnings
denied and all targets, plus test compilation in fixture and ordinary
configurations, on Linux. Missing cross tools, SDK inputs or failed compilation
fail the aggregate. Pushes to `main` and the daily nightly keep full Windows
execution and the existing native, VM and ignored-evidence allocations.

The formatting job also requires `scripts/release/licenses.py check`, using
the repository-pinned cargo-deny. The existing build-helper suite runs for
build-control changes and nightly; unresolved scopes run it as well. Existing
bench unittests run for full scopes or benchmark changes. Markdown documentation
under `docs/` and Markdown or text notice fragments under `notices/` can skip
compilation while retaining license validation. Referenced documents and unknown
inputs keep the conservative compilation scope.
Build-helper jobs install the existing CPU-priority user unit before exercising
real service cancellation and retain full Git history for legacy-controller
compatibility cases. Private RAM queue fixtures model their own memory and disk
capacity; they exercise allocation and locking without requiring a large tmpfs
or changing the production RAM limits. Nightly fixtures use private full-gate
locks and explicit fixture hardware rather than a machine's resource defaults.
The full helper discovery remains mandatory. Its platform-specific
`test_windows_native.NativeWindowsTests.test_cancellation_joins_live_child_before_a_second_waiter_acquires_the_lease`
case requires native Windows lease and job APIs and remains a named native
Windows check; a Linux skip does not qualify it. These fixture checks do not
replace deployment verification against the real scheduler and RAM governor.

The change detector uses the native gate's crate selection and direct consumers,
adding the law-test owners. Shared policy, core crates and unknown paths select
the full workspace. Only proved, unembedded user prose skips compilation. An
unavailable diff or dependency graph runs all ordinary checks.

Every tool job installs the same frozen corpus data as the native gate, including
archive consumers and doctests. The input is downloaded from the repository's
`ci-fixtures-<source-commit>` release and checked against the SHA-256
pinned in `.github/workflows/ci.yml`; `scripts/ci/fixture-input.toml` names the
source pin. An absent or mismatched input fails the job.
This bundle retains unchanged corpus and generator files, omitting documentation
and operational bookkeeping. It carries no database clones. Changing the native
fixture pin requires replacing the input bundle and its checksum together.
The download uses the run token. The bundle stays outside the workspace and
artifacts; installation never prints its contents.

The published corpus bundle is sanitized for public distribution, retains no
database clones or machine-specific paths, and is pinned by SHA-256 in the
workflow files.

Linux and Windows MSVC Clippy retain the gate's strict warnings, targets and
feature variants. Compiler caches have distinct role, platform and selected
feature keys, so lint, archive and doctest jobs cannot race to save one cache.
Each selected platform builds one nextest archive with the gate's acceptance
features. Its two shards start when that platform's archive is
ready, independently of other platforms. Selected integration targets supply
their ordinary CLI and server binaries in the same build; only out-of-scope
siblings need a separate build with the selected feature graph. Two shards
consume the archive without rebuilding its test binaries.
The shipped sibling CLI and Product are retained for the real process fixtures;
TUI journeys use their existing staged-Product input rather than rebuild it in
each shard. Checkout and
target paths must match between producer and consumers because existing tests
embed executable and source paths at compile time. Doctests run separately, as
does the gate's ordinary crash-fixture guard on supported platforms.
Consumers fetch the locked Cargo registry inputs before running offline compiler
probes, and retain full Git history for the module-size ratchet. The target
directory is absolute in every job, including checkpoint fixtures.

Linux jobs use the standard Ubuntu 26.04 runner: the plugin supervisor requires
[systemd 259's OOM counter](https://freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html).
Test consumers install bubblewrap and sccache for the
real containment and developer-tool fixtures. No plugin failure is exempted.

Landing runs use the exact slow-test tags in `scripts/build/slow_tests.py`.
Scheduled runs and manual `tier=nightly` runs include those tests. Ignored tests
retain their original execution and input requirements and are never promoted to
passes. Manual runs can request macOS, as can the `ci:macos` PR label; scheduled
runs also include it. All jobs use standard runners and hard job deadlines.

## Windows coverage still retained on native machines

Hosted Windows currently lacks the MXC security environment API, and the
credential-protection helper has timed out before Product readiness on a cold
runner. These failures remain visible coverage gaps. CI does not increase a
timeout or infer standard-user security from the default administrator token.

Every archive emits `ci-results/test-allocation.json`, naming **each exact binary
and test**, its destination and its reason. The report distinguishes hosted, VM,
nightly and ignored cases. The two native MXC cases stay on the VM;
credential-helper consumers are conservatively retained there until support is
established. The already measured public coding-tool path cases continue to run
on hosted Windows. Other tests without that helper dependency also run there.
An affected scope with no hosted cases reports that allocation explicitly.

The separate doctest artifact includes `ordinary-allocation.json`. Its Windows
VM rows name the ordinary handover guards
`ordinary_binary_ignores_the_fixture_crash_selector` and
`lifecycle::witnesses::ordinary_binary_ignores_the_fixture_lifecycle_selectors`.
Those ordinary binaries also bootstrap the protected Product process, so these
controls require native execution alongside the archived VM allocation.

Windows artifacts also record the runner's actual SID and administrator state.
Hosted green does not replace a required native run, standard-user credential
checks, release packaging, scheduler/helper qualification or other checks not
represented by this workflow. The native gate continues until replacement
coverage is explicitly approved.

## Runtime and cost

Each command prints its elapsed seconds. GitHub records job wall time, and each
shard retains its JUnit report and allocation. Build time and failed-test
completion rates must be reported separately from successful-case throughput.
Estimate compute from measured job minutes by OS; included usage and actual
charges depend on the billing account. macOS is opt-in for PRs to control cost.
Record workflow and job queued, started and finished times, attempt numbers,
wall time and compute separately.
For an unchanged candidate with an infrastructure failure, rerun failed jobs so
successful work is retained. A warm benchmark must confirm the role-cache hits;
compilation and test-execution timings alone do not prove a warm cache.
