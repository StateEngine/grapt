# Sandbox denials grants and escape controls

Build on the existing permission/receipt wiring and the Linux backend; every retry retains containment.

## S4 Denials and grants

[Harness Notes](https://harnessnotes.com/permissions#permissions-sandbox) separates permissions from containment. The consulted behavioral references report blocked hosts through a proxy (omit the unsandboxed retry) and separate approvals from containment, normalizing additional-permission requests with turn/session scope. Copy the resource preview/scoped amendment and the host feedback.

The permission dialog shows **“Sandbox denied write: <canonical path>”** or **“Sandbox denied connection: <host>:<port>”**, backend/reason, command, and **This run / This session / Always for this project / Deny**. File operations/proxy provide typed targets. Opaque shell errors retain raw text; `request_sandbox_access` supplies a labeled **requested target**, validated against policy. Stderr never authorizes grants. Protected Git/config/binary paths remain ungrantable.

Resolve paths and reject mutable aliases. Preview an exact file/directory; new-file creation needs an explicitly approved parent-directory grant. Never infer a parent or wildcard. Hosts bind exact hostname, transport and port, with DNS/rebinding checks.

Reuse the permission owner/store: run grants bind one retry capability; session grants occupy its bounded map until session end/restart; Always persists project-scoped sandbox resources. Saved rows require fresh policy resolution. `/permissions` lists/revokes grants; acknowledge revocation after stopping affected commands/connections and blocking new access. Deny floors win.

**Grant and retry** warns that earlier effects may repeat. Reap the failed process, revalidate identity/policy, mint a new capability and add only the selected resource. Preserve carveouts and seal actual enforcement; no unattended replay.

Host grants need a default-deny broker: Linux uses an isolated namespace/inherited channel/relay; Seatbelt permits only the proxy endpoint; MXC allows only its IP/port. Direct sockets stay blocked. Unsupported enforcement returns **“Sandbox cannot enforce this host grant”**; never flip today’s boolean to unrestricted networking.

## S6 Escape attempts

Reuse S1’s shell/PTY journey on Linux/bwrap, macOS/Seatbelt and Windows/MXC Medium. Use disposable fixtures with host-write positive controls. Proposed normalized refusals retain native EROFS/EACCES, EPERM or Windows access-denied details.

| Attempt on each OS | Probe | Expected refusal |
| --- | --- | --- |
| Missing config | `Path(missing_config).write_text('escape')` | `Sandbox denied write: <path>`; remains absent |
| Symlink into protection | POSIX `os.symlink`; Windows directory junction via `mklink /J`; write through alias | `Sandbox denied write: <protected target>`; unchanged |
| Hard link | `os.link` on one volume, before launch and during execution; write alias | `Sandbox protected alias: <path>` at preflight or denied write; unchanged |
| Agent-chosen folder | `mkdir`/write outside roots; change cwd/TEMP too | `Sandbox denied write: <path>` |
| Own binary | Truncate an idle executable copy in the protected installation directory | `Sandbox denied write: <binary>`; unchanged; running-image locks cannot supply the proof |

Repeat after ancestor rename, concurrency and cancellation. Grant/retry one ordinary path/host; an ungranted sibling/host still fails. Check expiry/revocation. Missing backend or unavailable native fixture is unproven.

Apache-2.0). After QUIET END: Linux backend, Linux/Windows file grants, Linux/Windows escape suite, brokered host grants, then macOS. Unsupported host grants retain the explicit refusal meanwhile.
