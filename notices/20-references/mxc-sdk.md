## Microsoft Execution Containers

The Windows containment adapter pins `mxc-sdk` to `=1.0.1`, from
`microsoft/mxc` commit `fda29ac63bc306b7f97f3b86b4b63d44f7f40cf2`,
`src/mxc-sdk`. It is licensed under MIT, with its license retained at
`licenses/mxc-sdk-1.0.1/LICENSE.md`. The locked transitive dependency licenses
are included in the generated release license register.

Only the native process-container backend is used; optional microvm/WSLC
features are disabled. No MXC source or unsafe FFI is copied into Grapt.
