### Native syntax-highlighting regex backend

R uses `syntect` 5.3.0 and `two-face` 0.5.2 with the Oniguruma backend.
The exact-pinned `onig` 6.5.3 and `onig_sys` 69.9.3 dependencies own the native
implementation and FFI; Grapt introduces no unsafe code. Rust-Onig is MIT,
Copyright 2015 Will Speak, Ivan Ivashchenko and contributors. The bundled
Oniguruma library is BSD-2-Clause, Copyright 2002–2021 K. Kosako. Both notices
are retained in `licenses/rust-onig/` and included with release licenses.

The R build reserializes the linked grammar data from `two-face`,
`generated/syntaxes-onig-newlines.bin` at
`67b64e632ceefe70e5f20f3eb797d9e33560a11c`, into language dependency closures.
Modified by Grapt; Copyright 2023–2025 the two-face developers, MIT OR Apache-2.0.
The Syntect 5.3.0 representation is from `trishume/syntect`,
`src/parsing/syntax_set.rs` and `syntax_definition.rs` at
`e4670846ecf16d8832db6c43d531bec466214e27` (MIT; Copyright 2017 Tristan Hume,
Keith Hall, Google Inc and other contributors). Original dependency and bundled
asset notices remain in `licenses/dependencies/NOTICE.html`.
The build also selects the two active syntax themes from Syntect’s
`assets/default.themedump` at the same commit; modified by Grapt to keep
unexposed themes out of the runtime worker without changing either palette.

