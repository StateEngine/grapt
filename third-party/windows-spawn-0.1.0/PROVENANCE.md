# Windows spawn failure observations

Pinned source: `P4suta/windows-spawn` at
`ce8acdc5b276483a2f5c1794d4850d23bef98915`, crates.io `windows-spawn` 0.1.0
(MIT OR Apache-2.0). Both upstream licenses are retained.

SE Code adds scoped, bounded observations of failed native calls, preserving the
original I/O errors and raw Windows error codes. The process request records its
creation flags, job list and inherited-handle count. The existing FFI calls,
ownership, rollback and launch policy retain upstream behavior. The addition
introduces no unsafe code or native calls.

This patch supplies information absent from the released safe adapter. Restore
the registry dependency when upstream supplies equivalent observations.
