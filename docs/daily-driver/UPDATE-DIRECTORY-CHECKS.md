# Linux installer directory checks

This is the directory-use sweep. Charter `8f84daf452` narrows the trust model:
the per-user prefix is trusted against same-user replacement. Check every
directory component with `symlink_metadata` immediately at the existing use
sites, and verify installed bytes before activation. These are corruption and
misconfiguration checks, not descriptor isolation or race hardening against a
process that can already replace the user's binaries. No rustix dependency,
unsafe code or new state owner is introduced. Windows junction checks remain
in the Windows slice; this Linux slice cannot claim Windows evidence.

The native baseline accepted an existing version-root symlink and activated
it. `/home/example/evidence/daily-driver/update-version-link-baseline-r1/witness.json`
records the actual package B source and exit0. The corrected genuine release package `7b7f7501` passed all nine native
directory cases in `update-snapshot-directory-native-r1/witness.json`.
Focused installation units and named directory mutants also passed; independent
review and Train30's full gate remain open.

| Existing directory/use | Check owner | Regression |
| --- | --- | --- |
| Prefix route before lock open and after acquisition; immutable binary route | service `directory`, `lock_file`, `identified`, `prefix_of_binary` | `linked_prefix_parent_refuses_guard_and_binary_selection` |
| Source package root and ancestors before verification/open; rechecked at copy | CLI `io::directory`, `io::regular`, `Package::verify`, staging `copy` | `substituted_package_parent_is_rechecked_when_inputs_are_copied` |
| Existing version root and bin before reuse | staging `install` plus `Package::verify` | `linked_existing_version_and_bin_refuse_before_reuse` |
| Versions and partial roots before create, read, cleanup and publication | staging `directory`, `remove_partial`, `sync_tree`, `install` | `linked_versions_and_partial_refuse_without_removing_outside_bytes` |
| Partial bin link during bounded cleanup | `remove_partial` unlinks the link itself, never treats it as a directory; new staging bin is real | `partial_bin_link_is_unlinked_without_following_outside_contents` |
| Pointer target version/bin before selection or recovery | transition `pointer`, CLI start `prepare`, regular input parent check | `linked_pointer_targets_refuse_and_preserve_transition_records`; prefix-selection regression |
| Record parent before create/replace/history append and directory sync | `io::write_new`, `replace`, `sync_directory`, transition `history` | `linked_parent_refuses_record_creation_and_directory_sync` |
| Actual installed binary content before intent/activation | `commands` prepares verified Package snapshots before prefix EX; `activate_verified` and `recover_verified` cheaply recheck retained identities/metadata under EX | `changed_installed_bytes_refuse_before_activation` |
| Quarantine | Not implemented in Linux slice1; prune later | Explicit deferral, no deletion credit |

`current` itself is the ruled relative version symlink. The parser permits only
`versions/<lowercase40hex>` and checks the real target directories; it is not
confused with a directory link in a reused path. Component inspection is
bounded to64. Staging removal remains bounded to1024 entries/depth10, and a
non-directory symlink is removed without following it.

Named counterexamples UPR-DIRECTORY, UPR-PREFIX-DIRECTORY and
UPR-ACTIVATION-DIGEST are included in the 19 current RED→GREEN unit results
in `update-snapshot-mutants-r1/receipt.json`. The compiled actual-package
matrix refuses linked version/bin/versions/partial/prefix-parent and pointer
targets before effects, while partial-bin cleanup and verified real-directory
reuse succeed without touching outside bytes. Its linked-version baseline
uses the actual older installer and activates the bad route, establishing
discrimination. The interrupted9bd affected suite supplies no qualification.
The remaining affected suites,
including whole product dogfood_headless and TUI dogfood_journey, are assigned to Train30.
