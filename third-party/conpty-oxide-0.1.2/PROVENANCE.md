# ConPTY spawn failure observations

Pinned source: `P4suta/conpty-oxide` at
`c954a31b86c8b418ca024d5ac1682e7f6f633e35`, crates.io `conpty-oxide` 0.1.2
(MIT OR Apache-2.0). Both upstream licenses are retained.

SE Code records the specific failed console creation, anonymous-pipe creation,
root-watcher handle duplication or wait registration in the existing Windows spawn observation scope. This distinguishes
these failures after process creation from creation refusals. Original errors,
native calls, ownership, rollback and lifecycle behavior retain upstream behavior;
the addition introduces no unsafe code or native calls.

Restore the registry dependency when upstream supplies equivalent observations.
