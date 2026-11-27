# ACP compatibility results

Campaign: `ACP-CLIENT-COMPATIBILITY-01@r1`

Base: `cb15c12c17b661e3f6a0a6ab78f985ac6850d0e8`

Candidate: `81965e0d90281ba39e85b6d6d14fe74172c0b47f`

Tree: `3940f8d92a2efd1d61aaa029db194bbdaf72624d`

At final evidence capture canonical `dev` remained the accepted base, and the
candidate descended from it without composition from another lane.

## Baseline counterexample

The governed Linux accepted-base run `20260915221237-09515e8cc4cf42f5` built the exact base and drove the official initialize frame while stdin remained open. The process stayed alive for the one-second observation window, emitted zero stdout bytes, then after EOF exited 2 and diagnosed the custom format as malformed. This proves the old executable was not an ACP editor peer.

- Receipt SHA-256: `50498a1c16274173e893ea3808acb6764447ec33b9ef2a4c4b9270e1892b2db1`
- Accepted binary SHA-256: `1c84aae0c0c8725b91213e6e0302b02644e076d64ca87d3c6dcdc2275e677567`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\20260915221237-09515e8cc4cf42f5`

## Implemented outcome

- Official JSON-RPC newline duplex replaces the retired custom batch decoder.
- Initialize responds before EOF; requests, notifications and peer callbacks remain concurrent and correlated.
- Durable lifecycle/configuration, MCP registration, catalog retry, prompts, events, permissions, forms, usage, tools and file sync use ordinary product owners.
- Product API now exposes narrow MCP-add, session-delete and session-fork connections required by the contract.
- Durable fork/delete and runtime MCP registration have explicit resurrection/eviction witnesses.

## Focused evidence

Before candidate freeze, these focused suites were green:

- `cargo +1.97.1 test -p grapt-cli --features loopback-testkit --no-fail-fast`: 7 unit, 12 contract and 12 public-seam tests at the recorded run; later additions are re-counted in the final gate.
- `cargo +1.97.1 test -p grapt-api --lib --no-fail-fast`: 86 passed at the route/API checkpoint.
- focused `grapt-product` runtime MCP, prompt attachment and credential-redaction tests: green.
- focused `grapt-session` fork/delete resurrection test: green.
- `cargo +1.97.1 clippy -p grapt-cli --all-targets --features loopback-testkit -- -D warnings`: exit 0; only the pre-existing workspace `clippy.toml` unreachable `getrandom::fill` configuration warning was printed.

These focused checkpoints are development evidence only. Final acceptance
evidence is pinned below to the immutable candidate.

## Final composition gate

Governed Linux run `20260916013837-a7f048f3c4184b0c` exercised exact candidate
`81965e0d` and tree `3940f8d9`. Scoped formatting, workspace clippy, workspace
rustdoc, workspace tests, dependency direction, compile-fail, store evidence,
event evidence and release evidence all exited 0. The ambient-clock and
ambient-randomness negative fixtures both exited 101 and matched
`clippy::disallowed_methods`, proving the two gates fired rather than silently
passing. Candidate custody was clean before and after the run; remote checkout
and process cleanup completed.

- Runner receipt SHA-256: `b08be89da6f051047f5ce2253679d249f8418992f825baaeca3f5a3b46a221f8`
- Returned receipt-file SHA-256: `eb6b001313b57eca969fa97988427fab046410357606b8cb5f4a72e705b446ed`
- Logs SHA-256: `735e33ac5abf349590804f996086a8dc2a86f5cab31de5cbf161c183bb110bab`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\20260916013837-a7f048f3c4184b0c`

The final commit also repairs two stale `grapt-types` rustdoc links found by
the composition gate. Governed accepted-base reproduction
`20260916013326-307f65d5a9eb4860` proved formatting and clippy green and rustdoc
red specifically at the unresolved `AuthorityLease` and `CapabilityId` links;
runner receipt SHA-256
`3e21615b49f9a0ccf60106c001c44244624d2c491eebbb590214c5019c57decd`.
The candidate run above proves the corrected workspace rustdoc green.

## Mutation evidence

Governed Linux run `20260916024359-f8bee66456c2401c` exercised the exact final
candidate. All 16 declared mutants (`ACP-T01`–`T04`, `E01`–`E04`, `C01`–`C02`,
`S01`, `M01`–`M02`, `P01`, `A01` and `D01`) produced a selected positive green,
the named failure red, and a restored green. The `ACP-Q01` reason-checker control
matched the actual `ACP-T01` red log, so a wrong-reason failure cannot release
the battery. The receipt reports unchanged HEAD and empty status; a post-run
author-tree scan found no stranded `MUTATED:` marker.

- Runner receipt SHA-256: `25e862b1c1436222691b07f4a64afb164c3fbfdf79e840847c077df8774dec09`
- Returned receipt-file SHA-256: `7a104ea23c55b13d0dd8c91c105e00cedc99cec0e93ab200ca6184713335a461`
- Mutation archive SHA-256: `384c99335d803a185ab9966cd7956c2548ae7be26b3dd6f318ec75078b11a060`
- Logs SHA-256: `2d637652484b685cde6b2dfeaa009adc3fee33595948a1ad8231a9337bfda141`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\20260916024359-f8bee66456c2401c`

## Packaged interoperability

### Governed Linux, exact final candidate

Run `20260916023942-9d6d8efe3a8f46d0` built production release artifacts,
assembled the shipped package, and drove one live ACP subprocess across
initialize, discovery, attach, ten requests, streaming, a server-to-client
permission callback, cancellation and final EOF. Stdin remained open across the
exchange. The independent witness-control inverted the pre-EOF assertion and
reported `ACP-CONTROL-RED` for its named reason. Candidate custody and remote
cleanup were clean.

- Runner receipt SHA-256: `223496d1de8f71c053e916e768d69fb61446e6abb7aff8f7242a330bbd3e6470`
- Returned receipt-file SHA-256: `f19c54764b7c5b3914ecf32ff3342931b2e4226e2063cba6abcae0ffa3aac5d4`
- Build receipt SHA-256: `31d3fd391584150c31672e0f0a2d4dd6e7722cdf8492971fa79ec2a39ba78755`
- Package manifest SHA-256: `93864d6ea9945793633a56f774b14d58c4badb45343c1c9705b32759528ee2ea`
- Journey receipt SHA-256: `7ca93a558eadfbfd9e7d65c2abd323ed3efbcedd079477235998567306ad304f`
- Control receipt SHA-256: `fe79f321b37ca4f5dbe5c55a87942448d4819975cad95e071dbe6cdaddc40f9c`
- Logs SHA-256: `b8f5e715e8e74867b135082dc878fe28e2838950fc25f5b2011a510a89f40272`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\20260916023942-9d6d8efe3a8f46d0`

### Windows artifacts and attended shipped-client witness

Exact-final-candidate run `20260916024206-74c1f282e1f9419a` cross-built and
returned the five production Windows binaries plus their build receipt with
matching remote/local hashes. Candidate custody and remote cleanup were clean.

- Runner receipt SHA-256: `b6058c26a18650b9b06dfb4659be5ed114c74960cc883442d69c89c03366dd7c`
- Returned receipt-file SHA-256: `eb89b4f729220d3c432a4495ccffb5d1add16d5bc75818723a2bce98feb52cc9`
- Product SHA-256: `dbbc243e9b52741e9d5efa5890a90c924f3306c00bb577d5e33253fde22f9ad7`
- CLI SHA-256: `46eb495abcffbbdafb2fadeb6cd689340a55fde39979e032f949242226eedd2b`
- TUI SHA-256: `9c50c390a71e5e5e9ecc4f5de43dfc72cdb8ab3e00aeaaede2df0ad6ace5d051`
- Migrate SHA-256: `9119709a2bde5817eb5419efb5f41ceba9ab1362730b4f460830f4e758bbac8b`
- Engined SHA-256: `2480dd3e1a33f4fc90f068109a292bbbcbfd17c5acbe332b1163470046d6d930`
- Build receipt SHA-256: `4ac4696b90b6a7f4919874aad571f39a5d6ddd3e94f702a51e3508c009d22d8f`
- Logs SHA-256: `69128a9aeb1c2df817da1ab8845c76040fc9846b465720048458509311d79551`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\20260916024206-74c1f282e1f9419a`

The attended Windows product journey used the already staged production package
from executable candidate `a4c9e3489200585a90feb470887fc18ac98ee180`
(manifest SHA-256
`15a83e105dfc8cbe1f84d904ce233c6d57f8c2dd56a4b3717cfc389d8c40a57f`).
With a child-only native Windows PowerShell module path, it passed package
identity, ordinary TLS provider, submission gate, first launch/isolation, the
same ten-request live duplex journey, permission callback, cancellation, EOF
cleanup and product/provider teardown. The run imported only fresh thumbprint
`B3B9985CEE066660BA6F1EB75BF71A82387A8BDB` into the invoking user's Root store
after attended consent, then removed that exact thumbprint after a second
attended consent. The receipt compared identity sets and reports no other root
appeared or disappeared. Independent post-run readback found the thumbprint and
stable journal absent, root count restored from 79 to the preflight 78, and the
CA signing key absent.

- Attended receipt SHA-256: `6eab2c8c77d69098398bbd13c79321388daa33932c45f744c89a09abc0e598dd`
- Evidence root: `E:\factory-evidence\acp-gamma-r1\windows-attended-a4c9e348`

This Windows behavioral witness is attributed to `a4c9e348`, not relabelled as
an exact-`81965e0d` execution. The final correction split large test/product
modules, restored a negative attachment fixture and repaired rustdoc links; its
exact Windows binaries therefore have different hashes. Exact-current Linux
operation, Windows production cross-build and the full composition/mutation
evidence are separate receipts above.

## Preserved non-credit attempts

Package run `20260916004323-1006f96fbea64b93` is non-credit: it refused with
`ARTIFACT_ABSENT` before candidate custody could be established. Package run
`20260916004941-4260b35fa6f84b01` is also non-credit: all command steps were
green, but the runner correctly refused `CANDIDATE_MUTATED` because outputs had
been written into the checkout. Both failed receipts and cleanup witnesses are
preserved. Candidate `a4c9e348` later moved package outputs outside the checkout;
its Linux package, Windows artifact and mutation receipts are valid historical
evidence but are superseded for exact-final-candidate credit by the runs above.

The first attended Windows attempt against the same staged `a4c9e348` package
failed closed before import because inherited PowerShell 7 module precedence
left Windows PowerShell 5.1 without the `Cert:` provider. It wrote no journal,
never called `Import-Certificate`, left its fresh thumbprint absent and preserved
receipt SHA-256
`9c37795104b442500c85cd7982250a8feacb77a2be76d9f28bdf51efb501993a`.
The successful run above changed only the child launch environment; it made no
global environment or profile change.

The first local ACP-T04 harness attempt at candidate `a6818be5` is explicitly retracted as mutation credit: the mutant was red, but its manifest expected a Rust slice-boundary panic while the byte-slice implementation correctly reached the witness assertion `diagnostic truncation split a UTF-8 code point`. The failed receipt is preserved outside the checkout as `failed-local-ACP-T04-a6818be5.tar.gz` (SHA-256 `cdb6c01f55ec4bda292bd27290fc0122828c35b24f2cf944f0370af3fa404e4b`); only a rerun with the corrected exact reason may count.

Governed Linux mutation run `20260916000054-44a5feee983d48b0` is also retracted as full-battery credit. ACP-T01–T04, E01–E02 and C01–C02 were valid green/red/restored triples, but ACP-S01 reached a generic session-ID assertion before the catalog-retry reason named by its manifest. Runner receipt SHA-256 `faf5ab6775bb95bdaf16cac60b140e295115a19f0f7841ffb2e1da94fedcecd8`; returned partial archive SHA-256 `ca8135631fc470f5bf2e5813524ba2d26851c266283b3a2255195e290fd74559`. The test now names the retry invariant at the first failing assertion; only a complete rerun may release the mutation tier.

Governed Linux mutation run `20260916000504-57a0031201c447e1` is likewise retracted as full-battery credit. ACP-T01–T04, E01–E02, C01–C02, S01, M01–M02, P01 and A01 were valid green/red/restored triples. ACP-D01 did turn red, but changing the emitted delete event to `session.forked` made the list projector reject the malformed payload before the named durable-fold assertion. Runner receipt SHA-256 `4d91a0328acdc9b5bb8907f2929829c0409417009e833f9e7abab113f597093c`; returned partial archive SHA-256 `dbea7f63e4cd5e91dd13a0b0601f8237721ab294e9153e94e69ccc8f6a8dfd23`. D01 now mutates the exact durable deletion predicate so the resurrection witness must fail for its named reason; only a complete rerun may release the mutation tier.

## Post-land corpus-pin correction

The `actions_sha256` value was correct when last written at `a6818be5`, then
`actions.jsonl` changed at `0f3ae2a5` and `83613e36` without updating the
manifest. Canonical-LF SHA-256 moved from `2622c8ab…` to `f9c565e2…` and then
to the landed `f4a3aedf…`. The ordered 14 scenario IDs and the declared bounds
did not drift.

No pre-landing gate, mutation run or package journey read this manifest pin;
those acceptance receipts therefore did not pass because of the stale value.
That missing consumer is why the drift survived. The corrected pin is now read
by a shared bounded validator used by both package and mutation harnesses. It
normalizes CRLF to canonical LF before hashing, verifies the ordered scenario
inventory, and runs an in-process wrong-pin control which must be rejected for
`actions_sha256 mismatch`.

## Launch recipe

Start or discover the ordinary Grapt service, then point an ACP-capable editor at:

```text
grapt acp --service <service-registration-path>
```

The editor writes one JSON-RPC object per line and keeps stdin open. It must send `initialize` first with `protocolVersion: 1`. Session methods use absolute `cwd` values. If the editor advertises `clientCapabilities.fs.writeTextFile: true`, Grapt may issue `fs/write_text_file` requests after completed edit tools. Diagnostics are written to stderr; stdout is protocol-only.

## Remaining external boundaries

- No paid inference, real credentials, OAuth acquisition or production deployment is used by this campaign.
- MCP SSE is not advertised. Remote MCP HTTP declarations register through the ordinary owner, but actual remote MCP execution remains an explicit existing product limitation rather than a false connected state.
- The attended Windows behavioral receipt is source-attributed to `a4c9e348`; exact `81965e0d` has production cross-build evidence but no second attended trust-store execution. That boundary is explicit rather than silently treating different binary hashes as identical.
- Final cold review is independent; corrections remain campaign work until the accepted candidate lands.
