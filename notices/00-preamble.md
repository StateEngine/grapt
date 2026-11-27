# Third-party notices

Grapt's own code is dual-licensed under the [MIT License](LICENSE-MIT) OR the
[Apache License, Version 2.0](LICENSE-APACHE), at your option. Files adapted from
Codex and Grok Build retain their Apache-2.0 notice obligations; OpenCode-derived
parts retain the MIT notice.

This register tracks source code copied or adapted into Grapt. Cargo and bundled
runtime dependencies also need their own applicable license texts in release packages.
The projects below are approved reuse sources; listing them is not an adoption.
Actual source adoptions are listed below.

## Separately licensed bundled component

Packages that include StateEngine carry its own runtime and Node under
`stateengine/`. StateEngine is a separate public project (currently
AGPL-3.0). Each bundled version is governed by the LICENSE and third-party
notices shipped with that version: read `stateengine/LICENSE` and
`stateengine/THIRD_PARTY_NOTICES.md`, including its Node license, before
distributing or using that component. Grapt's MIT OR Apache-2.0 grant does
not apply to that directory. The release manifest states whether it is
included and identifies its version and source revision.

## Approved sources

| Project | Inspected revision | License | Upstream notices |
| --- | --- | --- | --- |
