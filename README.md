# Grapt

Grapt is a coding-agent harness with a native terminal, a headless CLI and an
HTTP API. Agents work in a repository using provider accounts, tools and
permission controls. Sessions retain their history so work can resume.

## Start coding

Extract the package, open a terminal in your repository, and run:

```sh
grapt
```

Choose your repository, account and model, then enter a task. See
[Start coding](docs/release-packaging/QUICKSTART.md) for executable paths,
account setup, permissions and keyboard shortcuts. The package includes that
guide as `QUICKSTART.md` beside its binaries. The
[daily driver guide](docs/daily-driver/TUI-DRIVER.md) and
[shell environment policy](docs/daily-driver/SHELL-ENVIRONMENT.md) cover ongoing use.

## Build from source

With [Rust](https://rustup.rs) installed, build the binaries from a checkout.
The toolchain pinned in `rust-toolchain.toml` installs itself on first use.

```sh
cargo build --locked --release -p grapt-cli -p grapt-product -p grapt-tui
```

This writes `grapt`, `grapt-server` and `grapt-tui` to `target/release/`. Keep the
three binaries in the same directory and put that directory on your `PATH`.

## Connect a model

Connect a hosted provider with its API key. When the provider's usual key
variable, such as `ANTHROPIC_API_KEY`, is set, `grapt login` uses it, unless you
pass endpoint or cloud setup or a service is already running; otherwise it asks
for the key. `--key-stdin` reads a key from standard input instead.

```sh
grapt login anthropic
```

To use a model server on your own machine, name its preset. Grapt connects to
the server's usual address and offers every listed model that can call tools and
return text:

| Server | Command | Address |
| --- | --- | --- |
| Ollama | `grapt login ollama` | `http://127.0.0.1:11434/v1` |
| LM Studio | `grapt login lmstudio` | `http://127.0.0.1:1234/v1` |
| vLLM | `grapt login vllm` | `http://127.0.0.1:8000/v1` |
| llama.cpp server | `grapt login llamacpp` | `http://127.0.0.1:8080/v1` |
| Any OpenAI-compatible server | `grapt login custom --base-url <url>` | yours |

Use `--base-url` for another address, and `--model <id>` to connect one model by
name. A local server needs no API key. Grapt reads each model's context window
from the server. A window of 32K tokens can hold a typical first request, though
large project instructions or a long prompt may not fit, and each request also
reserves 16K tokens for the reply, so a small window compacts the conversation
often. Serve the model with 64K tokens or more for longer sessions.
For Ollama, start the server with `OLLAMA_CONTEXT_LENGTH=65536 ollama serve`.

## Run a task

In a repository, run one task and print the reply:

```sh
grapt run "summarize this repository"
```

`grapt run` uses your default profile and starts its service when none is
running. Run `grapt` for the interactive terminal.

Headless tasks can use [step limits](docs/headless-step-limits.md) to bound their work.

For new headless runs without session/harness persistence, see
[ephemeral sessions](docs/ephemeral-sessions.md). Existing persistent sessions can
be conservatively tombstoned with [offline retention cleanup](docs/session-cleanup.md);
cleanup is not physical erasure.

## Develop

This Rust workspace contains the agent runtime, providers, tools, durable store,
server, CLI and terminal client. The toolchain is pinned in `rust-toolchain.toml`.
Follow [project instructions](AGENTS.md) and
[Build, review and land](docs/BUILD-AND-LAND.md) for development and validation.
[Release 0.1](docs/RELEASE-0.1.md) tracks the daily-driver acceptance criteria.

## Moving a Linux worktree

Set `SECODE_WORKTREE_PARENT` to the directory for build worktrees. To relocate an idle worktree,
stop its jobs and ensure the destination does not exist, then run:

```sh
mv <old-path> <new-path>
git -C <main-checkout> worktree repair <new-path>
cd <new-path>
bash scripts/build/configure-worktree.sh --refresh
```

This procedure works across filesystems; `git worktree move` does not.
Refresh preserves customized Cargo configs, which require manual updates.
See [Build, review and land](docs/BUILD-AND-LAND.md) for gate and landing commands.

## Operator configuration

Build and maintenance helpers read `SECODE_DATA_ROOT`, `SECODE_WORKTREE_PARENT`
and `SECODE_CACHE_DIR` for data, checkouts and compiler artifacts. Defaults are
relative to the current user's home. Python helpers, shell entry points and PowerShell launchers load
plain unquoted `NAME=value` rows from `~/.config/secode/build.env`; explicit
environment variables take precedence. Keep host-specific files on each host. Set `SECODE_NVME_ROOT` to the build mount;
a dedicated mounted cache can also identify it. Installed user timers read
`~/.config/secode/build.env`; keep deployment values outside this repository.

Remote operators use `SECODE_WINDOWS_HOST`, `GRAPT_LINUX_HOST`,
`SECODE_CANONICAL_REMOTE` and `SECODE_CANONICAL_REPOSITORY`. Native qualification
also needs `SECODE_WINDOWS_GATE_ROOT` and `SECODE_WINDOWS_ORACLE_ROOT`. The
landing wrapper accepts its existing explicit parameters or the corresponding
`GRAPT_LINUX_HOME`, `SECODE_WORKTREE_PARENT`, `GRAPT_NODE_ROOT`,
`SECODE_STATEENGINE_PAYLOAD_ROOT` and `SECODE_LANDING_CONTROLLER` settings.
Remote mail uses `STATEENGINE_ENTRY` and `STATEENGINE_PROJECT_ROOT` on its host.
Set `SECODE_COMPUTE_PROFILE_DIR` to a directory containing deployment-specific
copies of the checked-in profiles; launchers retain their command-specific profile
selection. The checked-in profiles contain example locations and host aliases.

Evidence runs require explicit `SECODE_FIXTURES` and `SECODE_EVIDENCE_ROOT`
(or `REMOTE_COMPUTE_ARTIFACT_ROOT`) directories. Missing evidence fails rather
than selecting an operator's checkout.

## License

Grapt's own code is dual-licensed under the [MIT License](LICENSE-MIT) OR the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

Upstream adaptations retain their copyright and license obligations. See
[Third-party notices](notices/) (assembled into `THIRD_PARTY_NOTICES.md` in release packages) for attribution and applicable terms.
