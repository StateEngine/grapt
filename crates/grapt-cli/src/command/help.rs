//! Focused, effect-free command help.
//!
//! Help-before-dispatch.
//! Copyright 2025 OpenAI. Modified by Grapt: closed exact argv forms, no Clap.
//! License and NOTICE.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Status,
    ServiceStatus,
    SessionRename,
    Run,
    Attach,
    Acp,
    Login,
}

/// Recognize only standalone help forms; never reinterpret an option value.
pub fn parse(command: &str, rest: &[String]) -> Option<Topic> {
    let (topic, flags) = match command {
        "status" => (Topic::Status, rest),
        "run" => (Topic::Run, rest),
        "attach" => (Topic::Attach, rest),
        "acp" => (Topic::Acp, rest),
        "service" if rest.first().is_some_and(|value| value == "status") => {
            (Topic::ServiceStatus, &rest[1..])
        }
        "session" if rest.first().is_some_and(|value| value == "rename") => {
            (Topic::SessionRename, &rest[1..])
        }
        "login" if rest.first().is_some_and(|value| value == "chatgpt") => {
            (Topic::Login, &rest[1..])
        }
        "login" | "logout" if rest.first().is_some_and(|value| !value.starts_with('-')) => {
            (Topic::Login, &rest[1..])
        }
        "auth" if rest.first().is_some_and(|value| value == "status") => (Topic::Login, &rest[1..]),
        "login" | "logout" => (Topic::Login, rest),
        _ => return None,
    };
    (flags.len() == 1 && matches!(flags[0].as_str(), "--help" | "-h")).then_some(topic)
}

impl Topic {
    pub fn text(self) -> &'static str {
        match self {
            Self::Status => STATUS,
            Self::ServiceStatus => SERVICE_STATUS,
            Self::SessionRename => SESSION_RENAME,
            Self::Run => RUN,
            Self::Attach => ATTACH,
            Self::Acp => ACP,
            Self::Login => LOGIN,
        }
    }
}

const LOGIN: &str = "\
usage: grapt login [provider] [--profile <profile>] [options]
       grapt logout <connection> [--profile <profile>] [--json]
       grapt auth status [--profile <profile>] [--json]

Sign in to a new or existing profile. Changes apply immediately to a running
profile; an offline profile picks them up when started.
Omitted --profile means the default user profile. Login without a provider lists
the bundled providers.

ChatGPT uses browser OAuth. --no-browser selects the device flow for a headless
machine and displays its URL and code. Anthropic and other implemented API
providers use a key entered without terminal echo, --key-stdin, or
--api-key-env <name>. With none of these, the provider's usual variable is used
when it is set. Environment references store only the exported name; the
variable must be available to the service. Use openai for an OpenAI API key.

--id <id>       Select or create a named connection.
--label <label>  Name the connection.
--base-url <url> [--model <id> [--context-window <tokens>]]
                Set explicit coordinates for account-specific or local endpoints.
                Local presets (ollama, lmstudio, vllm, llamacpp) default to the
                server's usual address and need no key. Without --model, every
                model the server lists is offered, each at the window it reports.
Cloudflare AI Gateway requires --cf-account <account>, --cf-gateway <gateway>
and --cf-provider <provider>, with its API token entered at the prompt or stdin.
OpenAI/Anthropic need AI Gateway Run and stored provider keys or Unified Billing.
Other providers need Workers AI Read and Cloudflare credits.
--json, --jsonl  Emit JSON progress (pending, signed_in, failed with reason).
-h, --help      Show help without connecting.

Stable exits: 2 invalid command usage; 3 connection not found; 4 operation refused;
10 discovery failed; 11 incompatible protocol; 12 remote unavailable;
13 authentication failed (including a rejected key); 71 child failed;
74 output failed; 130 cancelled. HTTP 404 exits 3, HTTP 401 exits 13, other
HTTP 4xx exit 4, and HTTP 5xx or request transport failures exit 12.
The diagnostic never includes the key.

Auth status shows backend, key suffix and expiry or missing environment. It exits
1 when none is usable. Logout disconnects locally and does not revoke provider access.
Use the connection ID shown by auth status to disconnect a named connection.
Logout refuses a name that is not signed in and lists signed-in IDs, or none.

Examples:
  grapt login chatgpt --no-browser --json
  grapt login anthropic --api-key-env ANTHROPIC_API_KEY
  grapt login ollama
  grapt login openai --key-stdin --profile <profile>
  grapt auth status --json";

const SESSION_RENAME: &str = "\
usage: grapt session rename <id> <title> [connection]

Rename an existing Session without starting or interrupting its work.
Quote a title containing spaces. Titles are trimmed, nonempty printable lines
of at most 100 characters. A manual name is kept when later prompts arrive.

Connection (omitted means the running service in the default user profile):
  --service <root>                         Discover an existing local service.
  --remote <https-url> --credential <file>  Remote service; credential from file.
  -h, --help                              Show help without connecting.

The TUI also accepts /rename <title>; the name appears in its header and
Session list. New Sessions receive a title from their first nonempty prompt.

Examples (replace placeholders):
  grapt session rename <session-id> \"Investigate cache misses\"
  grapt session rename <session-id> \"Cache fix\" --service <profile>";

const STATUS: &str = "\
usage: grapt status --service <root> [--json|--jsonl]

Query readiness of an existing service; never starts a service.
  --service <root>  Required service discovery root.
  --json           Structured service status receipt.
  --jsonl          Structured service status receipt as a JSON line.
  -h, --help       Show this help without opening the service root.

Text output is `ready <endpoint>`. JSON and JSONL are mutually exclusive.
Absent, stale, incompatible or unreachable services refuse with a discovery
failure (exit 10), rather than printing `stopped`. Use `service status` for
its compatibility text query; it does not offer JSON or JSONL.

Examples (replace placeholders):
  grapt status --service <service-root>
  grapt status --service <service-root> --json";

const SERVICE_STATUS: &str = "\
usage: grapt service status --service <root>

Compatibility query of an existing service; never starts a service.
  --service <root>  Required service discovery root.
  -h, --help       Show this help without opening the service root.

Text output is the service URL on one line, or `stopped` for an absent or
stale service (both exit 0). Other discovery failures still refuse (exit 10).
JSON and JSONL are not supported. Use `grapt status` for a readiness query
(`ready <endpoint>` or a structured receipt with --json or --jsonl); that
query refuses absent or stale services instead of printing `stopped`.

Example (replace placeholder):
  grapt service status --service <service-root>";

const RUN: &str = "\
usage: grapt run [<connection>] [session] [<message>... | --prompt <text> | --stdin] [options]

Run one prompt noninteractively. Supply text or at least one --image.
--max-steps <positive-integer> caps logical provider calls for this input.
Retries share a Step; stricter runtime limits still apply. The cap persists with
the prompt, includes tool continuations and joined steers, and preserves completed
output when exhausted. It cannot be combined with --resident.
Text input sources are mutually exclusive.
Message words are joined; use `--` before literal flag-shaped message words.
--stdin reads bounded UTF-8 through EOF. Help does not read stdin or connect.
--resident with --jsonl keeps the client attached across idle for later mail or
inputs on the same Session. It admits the initial prompt once and exits on a
termination signal, service failure or the ordinary output bound. Closing its
JSONL reader abandons that stream, not the attachment; query durable state through
the service. The launcher still owns process detachment and exit monitoring.
It conflicts with --background.
--input-id <msg_KEY> sets the initial prompt's caller-owned identity. Requires an
exact --session without --fork. Transport uncertainty never triggers input replay;
the caller must reconcile durable acceptance against the same service and Session.

Connection (choose at most one; omitted means the default profile, whose
service starts when none is running):
  --service <root>                         Discover an existing local service.
  --remote <https-url> --credential <file>  Remote service; credential from file.
  --embedded --service <root> --product <binary>  Start a temporary local product.

Session (choose at most one; omitted means new):
  --new
  -m, --model <provider/model>             Select for a new session only.
  --agent <id>                            Select for a new session only.
  -s, --session <id>                       Exact existing session.
  -c, --continue, --resume                 Most recent session.
  --project <id>                          Only with an existing/resumed session.

Output (choose at most one; default is text):
  --format default | --json | --jsonl
  --thinking                              Include reasoning in text output.
  --reasoning-effort <level>               Request reasoning effort.
  --format json is refused; use --json or --jsonl instead.
  JSONL emits one initial history page, then session.messages.upsert records
  for changed completed messages and new notices or inputs. Partial assistant
  updates wait for completion or the run's terminal read.

Other run options:
  -i, --image <path>       Attach an image (repeatable, at most four).
  --ephemeral             Memory-only session/harness state; foreground headless only.
                          Requires --embedded --service <existing-provider-profile>
                          --product <binary> and a new session. No --project, fork,
                          resume/session, background, resident or worktree.
  --fork                  Requires --session or --continue/--resume.
  --background            Persistent service only; no client auto-approval.
  --worktree <new-directory>  New local session only; no remote, resume or fork.
  --title <name>          Name the run's Session (a nonempty line of at most 100 characters).
  --inspect | --author | --maintainer | --dangerously-bypass-permissions
                          Permission preset; strictest selected wins.
  --auto, --yolo          Approve classified requests once, except file removals
                          (including removals through patch or another tool).
  --dangerously-skip-permissions  Approve classified requests once, including
                          removals (dangerous).
  Both automatic modes reject owner-decision and unclassified requests;
  those require an explicit decision.
  -h, --help             Show this help without input, provider or service work.

Ephemeral mode reads existing provider configuration without renewing credentials
or writing session, harness, artifact or service-discovery state. It skips CLI
installation preparation; the profile must already exist. Intentional repository
edits and explicit exports remain user effects, not ephemeral session storage.
This is not secure erasure: provider retention, OS swap/crash dumps, shell history
and stdout/stderr capture by your caller are outside this guarantee. Tool execution
may be restricted where its harness cannot remain in memory. No interactive mode.

Examples (replace placeholders):
  grapt run --ephemeral --embedded --service <provider-profile> --product <binary> --prompt \"review this code\"
  grapt run --service <service-root> --prompt \"summarize the repository\"
  grapt run --remote <https-url> --credential <credential-file> --stdin --jsonl
  grapt run --service <service-root> --continue --fork -- explain the last change";

const ATTACH: &str = "\
usage: grapt attach [<connection>] (--session <id> | --resume | --new) [--project <id>] [--json|--jsonl]

Attach to a session without submitting a prompt.
Connection (choose at most one; omitted means the default profile, whose
service starts when none is running):
  --service <root>                         Discover an existing local service.
  --remote <https-url> --credential <file>  Remote service; credential from file.
  --embedded --service <root> --product <binary>  Start a temporary local product.

Session (choose exactly one):
  -s, --session <id>                       Exact existing session.
  -c, --continue, --resume                 Most recent session.
  --new                                   Create a session.
  --project <id>                          Only with an existing/resumed session.

Output defaults to text; --json and --jsonl are mutually exclusive.
No prompt, --stdin, --format, reasoning or other run-only options are accepted.
-h, --help shows this help without reading stdin, credentials or service state.

Examples (replace placeholders):
  grapt attach --service <service-root> --session <session-id>
  grapt attach --remote <https-url> --credential <credential-file> --resume --jsonl";

const ACP: &str = "\
usage: grapt acp [<connection>]

Serve the ACP protocol on stdin/stdout, not a prompt-oriented run command.
Connection (choose at most one; omitted means the default profile, whose
service starts when none is running):
  --service <root>                         Discover an existing local service.
  --remote <https-url> --credential <file>  Remote service; credential from file.
  --embedded --service <root> --product <binary>  Start a temporary local product.

Sessions and input are selected by protocol messages, not CLI session or prompt
flags. No --stdin, --json, --jsonl, --format or run-only options are accepted.
-h, --help prints this help and exits before reading protocol stdin, loading
credentials, connecting to a service or performing provider work.

Examples (replace placeholders; an ACP client supplies protocol stdin):
  grapt acp --service <service-root>
  grapt acp --remote <https-url> --credential <credential-file>";
