# Command sandbox

Native command isolation is opt-in while its release checks are in progress.
An unavailable backend refuses the command and records the reason. It never
silently falls back to a plain process or changes filesystem ACLs.

Windows uses MXC on Windows 11 build 26100.9278 or newer, at Medium integrity
without an administrator prompt. Linux uses bubblewrap as described below.

Enable it in your user configuration or profile's `grapt.jsonc`:

```jsonc
{
  "sandbox": {
    "enabled": true,
    "network": true,
    "writable_roots": ["E:/shared-build-cache"]
  }
}
```

The workspace, the server's temporary directory and these existing absolute roots
are writable. `.git/config`, `.git/hooks`, Grapt configuration and the directory
containing its binaries and terminal shim stay protected even inside a writable
root. A worktree's Git control paths are protected too. If a protected configuration
file has missing parent directories, the first missing directory is protected
against creation. The Windows system drive and drives
containing granted roots remain readable; this setting primarily restricts writes. Put tool caches in the workspace
or add their paths to your user-owned roots when needed.

Repository configuration may enable isolation or turn network access off. It
cannot disable a user policy, add writable roots or turn network back on. Changing
a command's environment does not add its chosen temporary directory to the policy.
The sandbox setting applies to both shell tools and to interactive and pipe-based
exec commands, independently of the permission preset.

Set `sandbox.git_metadata_read_only` to `true` for workspace commands that must
read Git metadata without changing it. This enables isolation even when
`sandbox.enabled` is false. A true value in any configuration layer stays true;
a later false value or omission does not remove it. With this setting, the
complete resolved `.git` directory, a linked worktree's Git directory and common
directory, and their pointer files are protected. Creating a missing local
`.git` entry is blocked too. Ordinary workspace files remain writable. Absent or
false preserves the existing Git-control protection; it does not select a named
profile or grant permissions. Malformed or unresolved Git pointers refuse policy
resolution.

Before starting a service or Session, an embedding host can run
`grapt-server sandbox-policy --capabilities`. The JSON response identifies
`grapt.sandbox-policy.v1` and the `git_metadata_read_only/v1` capability. Reject
an older package that does not advertise the required capability before changing
its lifecycle. To inspect the effective policy with the same configuration fold
used by command execution, run
`grapt-server sandbox-policy --service <absolute-profile-root> --directory <absolute-workspace>`.
This reads configuration without initializing a profile, database or service.
The response contains resolved roots, protected paths and the effective
`git_metadata_read_only` value; `policy: null` means isolation is off.
`realization: "not_started"` reports intended policy, not a successfully contained
process. Configuration and filesystem identities are checked again at execution.

The read-only Git policy keeps MSYS commands contained even if a previous Session
compatibility grant exists, and refuses `msys_unconfined: true`. Select a profile
that permits Git writes explicitly if an unconfined command is required.

Each command result records its actual enforcement. The TUI tool row shows
`sandboxed`, or `degraded: <reason>` for an unavailable backend or an explicitly
approved compatibility fallback; an off setting adds no label.
A failed startup can leave the effect unknown, which the receipt records explicitly.
Check that result before retrying a command with side effects.

On the validated Windows MXC host, npm's `file:directory` dependencies are refused
with `EPERM` during link creation. Packed tarball dependencies install normally,
including lifecycle scripts. This restriction also applies when the same directory
link succeeds outside containment.

## Retrying a denied write

A shell error does not authorize extra access. The agent can explicitly call
`request_sandbox_access` with an existing absolute `path` and the exact `cmd`
it wants to retry. The permission dialog labels the requested target and command;
earlier command effects may repeat. New files require requesting their parent
directory explicitly. Protected paths and hard-link aliases cannot be granted.

Choose this run, this session, or Always for this project. Session and Always
remember only the exact target; command authorization remains separate. Each
retry keeps native containment and revalidates the target's identity. It cannot
write an ungranted sibling or turn network access on.

For network access, the tool accepts `host` instead of `path`. The current
backends return “Sandbox cannot enforce this host grant” and do not retry the
command or change network access. They never replace a host request with an
unrestricted network grant.

Use `/permissions` to inspect active run grants and remembered session/project
grants. Select a row and press Enter to revoke it. Grapt fences prepared
retries and stops affected commands before confirming withdrawal. A pending
withdrawal or unconfirmed cleanup remains explicit; retry revocation before
relying on it. Revoking a run leaves separately remembered consent intact.

## Linux

Linux uses `/usr/bin/bwrap` with unprivileged user, PID and mount namespaces and
seccomp. Install your distribution's bubblewrap package and enable unprivileged
user namespaces. If either is unavailable, the command is refused with
`degraded: <reason>`; Grapt never substitutes a plain shell.

The same user `sandbox` settings apply. Use Linux absolute paths for declared
roots. The filesystem is readable and read-only by default, with the workspace,
temporary directory and declared roots writable. Protected Git controls, Grapt
configuration, binaries and shim remain read-only inside those roots. Missing
protected paths cannot be created. A protected path crossing a mutable symlink
refuses startup.

Protected files with hard-link aliases and protected directory symlinks also
refuse startup. A file symlink cannot point into a writable root. Keep installed
binaries as regular files in a dedicated directory; a build-output directory may
contain aliases or exceed the protected-tree scan bound. Unreadable or oversized
protected trees are refused with a reason.

`network: false` isolates the network namespace and refuses socket creation and
connections, including curl, Node and raw-socket requests. `network: true` permits
IP networking. Host Unix-domain services are unavailable in either mode; local
socketpairs between related processes remain usable. Nested user namespaces are
disabled in contained commands.

Loader environment settings (`LD_*`, `GLIBC_TUNABLES`, `GCONV_PATH`, `LOCPATH`,
`BASH_ENV` and `ENV`) are removed before starting the trusted launcher. Set any
needed command-specific loader settings inside the contained command instead.

## Windows MSYS programs

MSYS2 and Cygwin programs, including Git for Windows' SSH, Bash and hooks, need
named objects that MXC cannot currently provide. For a recognized executable,
Grapt offers a separate MSYS compatibility permission. **Once** runs that command
and its children without OS containment. **Session** runs all subsequent Windows
shell commands and their children without OS containment, including MSYS programs
started inside PowerShell scripts. This removes the OS protected-path and network
restrictions for those command trees; ordinary command permission still applies.
The grant applies only to that Session and cannot be saved with Always. Existing
executable-specific grants do not widen into command-tree consent. Configuration,
permission hooks and Bypass mode cannot supply this grant. The permission explains
that the command and all its children run without OS containment, including the
protected-path and network restrictions. Its result is marked
`degraded: msys runtime needs a named-object namespace`, or
`degraded: session MSYS grant: command and all children run without OS containment`
for later command trees, also on terminal polls.
Rejecting the permission starts no process. Grapt never retries a failed command
outside containment automatically.

Without Session consent, use one literal MSYS command or explicitly select its
shell to request the compatibility permission. To request it for MSYS children
inside PowerShell or another native command, set `msys_unconfined: true` on
`exec_command` or `shell`. The flag only requests approval; it grants no access.
Once is the first choice and Enter selects it; later commands stay contained.
An MSYS descendant inside an ordinary
PowerShell command remains confined and may fail. Its result names the sandbox's
named-object denial and explains how to obtain Session consent; no effect is replayed.

An absent protected Git file, such as `config.worktree`, stays protected against
creation. Git may print `Permission denied` or exit 128 when
`extensions.worktreeConfig=true` and that file is missing. Grapt checks the
session repository at startup and before contained commands, and records a named
**Windows sandbox Git worktree config warning** in the conversation.

Create an empty file at `git rev-parse --git-path config.worktree` yourself,
outside the sandbox, or use these native commands:

1. `/sandbox worktree-config` previews the exact missing path without writing.
2. `/sandbox worktree-config create confirm` explicitly consents to creating only
   that empty file outside the sandbox. Grapt re-checks it afterward.

Closing the preview is not consent. Switching sessions or any creation attempt
requires a fresh preview. Existing files are never overwritten and sandbox deny
rules remain unchanged. A lost response is not retried automatically; inspect the
file and preview again before another attempt.

A contained command that reports the missing-file denial also names
`sandbox: protected path config.worktree (absent, creation blocked)` in its result.
Existing protected configuration remains readable; contained commands do not create
repository metadata to suppress the warning.
