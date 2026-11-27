# Shell environments

The agent's `shell` and `exec_command` tools inherit the service's parent
environment by default, including `PATH`, `HOME`/`USERPROFILE`, Rust toolchain
settings and Git configuration variables. This applies to pipes and terminals on
Linux and Windows. Start the service from the developer terminal whose tools you
want available; changing a separate terminal does not update a running service.

Credential variables are always removed, case-insensitively: any name containing
`KEY`, `SECRET`, `TOKEN` or `PASSWORD`, and every environment name used by a loaded
provider credential grant (`provider.credential: "env:NAME"`). This includes a
custom credential name without those keywords. Neither `set` nor `include_only`
can restore them. OAuth credentials held in files are never inserted into the
environment. This filtering controls process inheritance; shell tools still have
the filesystem access shown by their permission prompt.

Launcher identity is also always removed: `STATEENGINE_*`,
`SECODE_STATEENGINE_*` and `SECODE_SESSION_ID`, without regard to case.
Configuration cannot restore these names. `grapt service start` and `grapt code`
remove them before starting the service; a direct `grapt-server serve`,
`serve-terminal` or `handover` refuses to start with inherited launcher identity.
Each seat's identity and session capability must be provisioned explicitly.
The agent's `shell` and `exec_command` children then receive their own
`SECODE_SESSION_ID` and `STATEENGINE_SEAT_SURFACE=secode`, stamped after all <!-- grapt-rename: keep -->
configuration filters. Inherited `CODEX_THREAD_ID` and `CLAUDE_CODE_SESSION_ID`
are removed. A provisioned Session can therefore use `se` as its own seat.

Put `shell_environment_policy` in your project's `grapt.jsonc`:

```jsonc
{
  "shell_environment_policy": {
    "inherit": "all",
    "exclude": ["CI_*", "BUILD_NOISE"],
    "set": {"BUILD_MODE": "development"},
    "include_only": []
  }
}
```

`inherit` accepts `all` (default), `core` or `none`. `core` keeps native path,
user/home, temporary directory and shell startup variables; on Linux it also
keeps locale variables, and on Windows it keeps `PATHEXT`, system roots and app
data directories. `none` inherits nothing; set any required startup variables
explicitly, or choose `core` on Windows.

Names in `exclude` and `include_only` use case-insensitive globs: `*` matches any
sequence and `?` matches one character. Construction starts with `inherit`,
applies `exclude`, adds agent command defaults, applies `set`, then applies
nonempty `include_only`, and always
removes credentials and launcher identity, then stamps the executing Session.
An empty `include_only` keeps all remaining names.
Windows overrides replace existing names without regard to case; Linux names
remain case-sensitive. Values are literal strings, without variable expansion.

Agent commands default to `PAGER=cat`, `GIT_PAGER=cat`, `GH_PAGER=cat`,
`CI=1`, `GRAPT_CI=1` and, for one release, the pre-rename `SECODE_CI=1` so <!-- grapt-rename: keep -->
tools avoid interactive pagers and prompts.
They also receive `TERM=dumb`, `NO_COLOR=1`, empty `COLORTERM`, and
`LANG`, `LC_CTYPE` and `LC_ALL` set to `C.UTF-8`. These replace inherited values;
explicit `set` entries override them and `include_only` can remove them.
For colored command output, set `NO_COLOR` to an empty string and choose the
terminal settings you need. The defaults affect command children only, including
PTYs; they do not change the service or R TUI's rendering environment or hooks.

Configuration uses the same ordered sources as the other project settings:
service profile, user configuration, project ancestors and explicit overrides.
Later documents replace `inherit`, `exclude` and `include_only` when present;
`set` entries merge by name. Each call captures its environment before approval,
so editing configuration while approval is pending does not change that call.
Only names enter permission identity and diagnostics; values stay private.

Each filter accepts up to 64 patterns of 256 bytes; `set` accepts up to 64 entries.
A prepared environment holds at most 1,024 entries and 256 KiB, with names up to
256 bytes and values up to 32 KiB. Invalid settings or exceeded bounds refuse the
call. There is no option to disable credential filtering.

MCP servers, internal workspace-diff commands and codemode workers retain their
existing explicit environment isolation. Plugin integration commands also keep
their existing isolated environment.
