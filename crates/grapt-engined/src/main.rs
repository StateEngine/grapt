//! `grapt-engined` — the headless composition root.
//!
//! Two verb families and nothing else: `serve`, and
//! `service ensure|stop|status`.
//!
//! # What E1a actually closes, and what it refuses by design
//!
//! `serve` and `service status` are the whole of the E1a product cut.
//! `service ensure` and `service stop` are **refused by design** here, with a
//! typed refusal that names the missing authority and a non-zero exit: an
//! `ensure` writes a durable registration and needs a truthful production
//! `ClockAuthority`, and the only production clock and id adapters in the
//! workspace live in `grapt-store`, which this crate's frozen dependency
//! closure refuses. The lawful cure is to RELOCATE those adapters, which is the
//! adapter-relocation lane's work, and the verbs themselves are then owned by
//! the controller lane gated behind it. Neither verb is stubbed, faked or
//! silently degraded — a reader of this source learns the boundary here rather
//! than discovering it from a receipt.
//!
//! Every law this binary carries is a law about
//! *composition* — which bound was supplied, what the receipt said, which exit
//! disposition was rendered, where a receipt landed, what the process exit code
//! was. Every domain decision belongs to `grapt-api`, `grapt-location` or
//! `grapt-service`, and none of them is re-decided here.
//!
//! # The one bound this crate owns
//!
//! Every other bound here is decided by the crate that owns the law it bounds,
//! and this binary re-validates none of them. `--serve-window-ms` is the single
//! declared exception, and it is an exception because **no crate owns a process
//! run window**: there is no first carrier to duplicate. It is validated by
//! [`receipt::ServeWindow`], inside this crate, because adding it to
//! `ApiHostLimits` would be an edit to another crate that this lane refuses.
//!
//! The reason it exists at all is not that anything needs a way to stop the
//! host. This binary already refuses to run unbounded in every dimension it
//! has — ten numeric bounds, none defaulted, over types that implement no
//! `Default` and a compile-fail probe that keeps it that way. Its *lifetime*
//! was the last unbounded dimension. An operator interrupt races the window and
//! stops the host sooner; the window is the bound on an unattended run, not the
//! only way out of one.
//!
//! Platform admission uses exact Windows, Linux and macOS names in
//! [`platform::platform_support`]. Other operating systems refuse before
//! sockets, locks or registration reads; there is no broad Unix admission.

mod custody;
mod platform;
mod receipt;
mod service;

use grapt_api::auth::AuthenticationPolicyVersion;
use grapt_api::directory::{MaxPrincipalMappings, SeatPrincipalDirectory};
use grapt_api::router::{ApiRouterBuilder, MaxRouterModules};
use grapt_api::{
    ApiHostLimits, ApiHostReady, AuthenticatedApiService, HeaderReadAllowance, MaxConnections,
    MaxHeaders, MaxInflightRequests, MaxRequestBodyBytes, MaxRequestHeadBytes,
    MaxRequestsPerConnection, PreparedApiHost, SeatTrustBoundary, ShutdownDrainAllowance,
    StreamWriteAllowance, TokioDeadlineDriver,
};
use grapt_location::{
    BindHost, BindLimits, BindPort, BoundListener, ListenerBindPlan, ListenerBinder,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses,
};
use receipt::{ServeBounds, ServeReceipt, ServeWindow, StopTrigger};
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every bound is a required argument. None is ever defaulted.
///
/// `BindLimits` and `ApiHostLimits` deliberately implement no `Default` under
/// the bounded-everything rule. A binary that invented one would become a
/// second carrier of the law those types exist to hold, and an operator who
/// omitted a bound would get a silent number nobody chose.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArgError {
    /// The named flag was not supplied.
    Missing(&'static str),
    /// The named flag was supplied with a value that is not admissible.
    Invalid { flag: &'static str, value: String },
    /// A flag this binary does not define.
    Unknown(String),
    /// No verb, or a verb outside the frozen surface.
    UnknownVerb(String),
}

impl ArgError {
    fn detail(&self) -> String {
        match self {
            ArgError::Missing(flag) => format!("required bound {flag} was not supplied"),
            ArgError::Invalid { flag, value } => {
                format!("bound {flag} does not admit the value {value:?}")
            }
            ArgError::Unknown(flag) => format!("unknown argument {flag:?}"),
            ArgError::UnknownVerb(verb) => format!(
                "unknown verb {verb:?}; this binary defines serve and service ensure|stop|status"
            ),
        }
    }
}

/// What the invocation achieved.
#[derive(Debug)]
enum Success {
    /// The host bound, ran, and reported an exit disposition.
    ///
    /// `exit` is the host's own terminal receipt rendered by
    /// [`receipt::render_exit`] — never a literal this binary chose — and
    /// `trigger` names which of the two termination paths fired.
    Served {
        exit: &'static str,
        trigger: StopTrigger,
    },
    /// A service verb settled.
    Service(service::ServiceOutcome),
}

/// Why the invocation refused.
#[derive(Debug)]
enum Refusal {
    /// An argument was missing, invalid or unknown.
    Argument(ArgError),
    /// The host platform is not explicitly supported.
    Platform(String),
    /// A receipt root could not be settled.
    Custody(custody::CustodyRefusal),
    /// A service verb refused.
    Service(service::ServiceRefusal),
    /// The listener could not be bound.
    Bind(String),
    /// A structural constant of this composition root was refused by the crate
    /// that owns it, or the runtime it needs could not be built.
    ///
    /// Not reachable from any operator input — the values it covers are
    /// constants of E1a's own shape, an empty directory and an empty router —
    /// but a composition root that met one and pretended otherwise would be
    /// claiming a host it never built. It is a typed refusal so that the
    /// impossible case is still reported honestly and still exits non-zero,
    /// rather than being an `unwrap` waiting for a library to change its mind.
    Compose(String),
}

/// The result of one invocation.
#[derive(Debug)]
enum Outcome {
    Ok(Success),
    Refused(Refusal),
}

/// A refusal exits non-zero.
///
/// A refusal that exited zero would be indistinguishable from success to every
/// caller that checks an exit code — which is every supervisor, every CI step
/// and every shell `&&`. The receipt on stdout says the same thing, but a
/// process exit code is the only part of it a caller sees without parsing.
fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Ok(_) => 0,
        Outcome::Refused(_) => 2,
    }
}

/// Exactly one JSON object to stdout; human prose to stderr.
///
/// The split is the law. A caller reading stdout gets one parseable object or
/// nothing at all, and prose interleaved into that stream would make the
/// receipt unparseable for the one consumer it exists to serve.
fn emit(json: &str, prose: &str) -> io::Result<()> {
    writeln!(io::stdout(), "{json}")?;
    writeln!(io::stderr(), "{prose}")?;
    Ok(())
}

/// One `--flag value` pair, required.
fn required<'a>(args: &'a [String], flag: &'static str) -> Result<&'a str, ArgError> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            return args
                .get(index + 1)
                .map(String::as_str)
                .ok_or(ArgError::Missing(flag));
        }
        index += 1;
    }
    Err(ArgError::Missing(flag))
}

/// A required bound, parsed.
fn required_number(args: &[String], flag: &'static str) -> Result<u64, ArgError> {
    let raw = required(args, flag)?;
    raw.parse::<u64>().map_err(|_| ArgError::Invalid {
        flag,
        value: raw.to_owned(),
    })
}

/// Collect every declared bound for `serve`, refusing the first omission.
fn serve_bounds(args: &[String]) -> Result<ServeBounds, ArgError> {
    Ok(ServeBounds {
        max_connections: usize_bound(args, "--max-connections")?,
        max_inflight: usize_bound(args, "--max-inflight")?,
        max_headers: usize_bound(args, "--max-headers")?,
        max_request_head_bytes: usize_bound(args, "--max-request-head-bytes")?,
        max_request_body_bytes: usize_bound(args, "--max-request-body-bytes")?,
        max_requests_per_connection: usize_bound(args, "--max-requests-per-connection")?,
        header_read_allowance_ms: required_number(args, "--header-read-allowance-ms")?,
        shutdown_drain_allowance_ms: required_number(args, "--shutdown-drain-allowance-ms")?,
        bind_attempts: usize_bound(args, "--bind-attempts")?,
        max_resolved_addresses: usize_bound(args, "--max-resolved-addresses")?,
        serve_window: window_bound(args, "--serve-window-ms")?,
    })
}

/// The run window, refused at zero by the constructor that owns it.
fn window_bound(args: &[String], flag: &'static str) -> Result<ServeWindow, ArgError> {
    let value = required_number(args, flag)?;
    ServeWindow::try_new(value).ok_or(ArgError::Invalid {
        flag,
        value: value.to_string(),
    })
}

/// A required bound that is a count rather than a duration.
fn usize_bound(args: &[String], flag: &'static str) -> Result<usize, ArgError> {
    let value = required_number(args, flag)?;
    usize::try_from(value).map_err(|_| ArgError::Invalid {
        flag,
        value: value.to_string(),
    })
}

/// Reject any argument this binary does not define.
///
/// An unknown flag that is silently ignored reports a coverage the caller did
/// not ask for — the same defect the mutation runners refuse by name.
fn reject_unknown(args: &[String], known: &[&str]) -> Result<(), ArgError> {
    for argument in args {
        if argument.starts_with("--") && !known.contains(&argument.as_str()) {
            return Err(ArgError::Unknown(argument.clone()));
        }
    }
    Ok(())
}

/// The flags `serve` defines — and nothing it does not read.
///
/// `--registration`, `--slot` and `--instance` were accepted here and never
/// read by `run_serve`. That is the same defect [`reject_unknown`] exists to
/// prevent from the other side: an argument accepted and discarded reports a
/// coverage the caller did not ask for. `serve` takes no registration at E1a —
/// registration is `service`'s subject, and a `serve` that registered would need
/// the elected controller authority this lane refuses — so the honest surface is
/// the smaller one and the three are gone. An operator who passes one now gets
/// the same named refusal as any other undefined flag, rather than silence.
const SERVE_FLAGS: &[&str] = &[
    "--bind",
    "--receipt-root",
    "--max-connections",
    "--max-inflight",
    "--max-headers",
    "--max-request-head-bytes",
    "--max-request-body-bytes",
    "--max-requests-per-connection",
    "--header-read-allowance-ms",
    "--shutdown-drain-allowance-ms",
    "--bind-attempts",
    "--max-resolved-addresses",
    "--serve-window-ms",
];

const SERVICE_FLAGS: &[&str] = &[
    "--registration",
    "--slot",
    "--instance",
    "--receipt-root",
    "--lock-attempts",
];

/// The receipt-root override from before the product rename, still honoured.
const LEGACY_RECEIPT_ROOT_ENV: &str = "SECODE_RECEIPT_ROOT"; // grapt-rename: keep

/// Settle the receipt root for this invocation.
fn settle_receipt_root(args: &[String]) -> Result<PathBuf, Refusal> {
    let explicit = required(args, "--receipt-root").ok().map(PathBuf::from);
    let from_env = std::env::var_os("GRAPT_RECEIPT_ROOT")
        .or_else(|| std::env::var_os(LEGACY_RECEIPT_ROOT_ENV))
        .map(PathBuf::from);
    custody::receipt_root(explicit, from_env, custody::portable_receipt_root)
        .map_err(Refusal::Custody)
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let outcome = run(&argv);
    let code = exit_code(&outcome);
    let (json, prose) = render(&outcome);
    // A receipt that cannot be written is not a reason to claim a different
    // outcome, so a broken pipe changes the exit code of nothing.
    let _ = emit(&json, &prose);
    std::process::exit(code);
}

/// Render the single JSON object and its prose companion.
fn render(outcome: &Outcome) -> (String, String) {
    match outcome {
        Outcome::Ok(Success::Served { exit, trigger }) => (
            receipt::detailed_json("serve", "exit", exit, "trigger", trigger.label()),
            format!("serve finished: {exit} after {}", trigger.label()),
        ),
        // A successful observation renders WHAT IT SAW. `label()` alone is the
        // constant "observed" for all three states, so a receipt built from it
        // reports that a status ran and nothing about the slot.
        Outcome::Ok(Success::Service(result)) => (
            match result.observation() {
                Some((state, owner)) => receipt::observation_json(result.label(), state, owner),
                None => receipt::simple_json("service", "outcome", result.label()),
            },
            result.prose(),
        ),
        Outcome::Refused(refusal) => {
            let (label, detail) = refusal_parts(refusal);
            (
                receipt::detailed_json("refused", "refusal", label, "detail", &detail),
                format!("refused: {detail}"),
            )
        }
    }
}

fn refusal_parts(refusal: &Refusal) -> (&'static str, String) {
    match refusal {
        Refusal::Argument(error) => ("argument", error.detail()),
        Refusal::Platform(text) => ("unsupported_platform", text.clone()),
        Refusal::Custody(refusal) => ("receipt_custody", refusal.refusal_text()),
        Refusal::Service(refusal) => (refusal.label(), refusal.detail()),
        Refusal::Bind(text) => ("bind", text.clone()),
        Refusal::Compose(text) => ("composition", text.clone()),
    }
}

/// The whole invocation, as a value.
fn run(argv: &[String]) -> Outcome {
    let support = platform::platform_support(std::env::consts::OS);
    if let Some(text) = support.refusal_text() {
        return Outcome::Refused(Refusal::Platform(text));
    }
    let Some(verb) = argv.first() else {
        return Outcome::Refused(Refusal::Argument(ArgError::UnknownVerb(String::new())));
    };
    match verb.as_str() {
        "serve" => run_serve(&argv[1..]),
        "service" => run_service(&argv[1..]),
        other => Outcome::Refused(Refusal::Argument(ArgError::UnknownVerb(other.to_owned()))),
    }
}

/// `serve`: settle custody, take every bound, bind, and report.
fn run_serve(args: &[String]) -> Outcome {
    if let Err(error) = reject_unknown(args, SERVE_FLAGS) {
        return Outcome::Refused(Refusal::Argument(error));
    }
    let bounds = match serve_bounds(args) {
        Ok(bounds) => bounds,
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let receipt_root = match settle_receipt_root(args) {
        Ok(root) => root,
        Err(refusal) => return Outcome::Refused(refusal),
    };
    let bind = match required(args, "--bind") {
        Ok(value) => value.to_owned(),
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let Some(requested) = literal_bind(&bind) else {
        return Outcome::Refused(Refusal::Bind(format!(
            "bind address {bind:?} is not a literal ip:port; grapt-engined resolves no hostname"
        )));
    };
    serve_host(&bounds, requested, &receipt_root)
}

/// R11: a literal `ip:port` only, and `None` for everything else.
///
/// A hostname is refused rather than resolved, because resolution is a network
/// effect this binary does not take. The parse happens exactly ONCE and its
/// value is carried onward: a second parse whose failure produced the same
/// refusal would let a mutant of this rule behave identically to the rule, and
/// that is a survivor dressed as a red.
fn literal_bind(bind: &str) -> Option<SocketAddr> {
    bind.parse::<SocketAddr>().ok()
}

/// Build the runtime this host needs, and run it to its own terminal.
///
/// The runtime is built here rather than over `main` so that every non-`serve`
/// path — every argument refusal, every `service` verb — still runs with no
/// reactor at all. A binary that started a thread pool to report a missing flag
/// would be doing work its receipt does not mention.
fn serve_host(bounds: &ServeBounds, requested: SocketAddr, receipt_root: &Path) -> Outcome {
    let limits = match host_limits(bounds) {
        Ok(limits) => limits,
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let bind_limits = match bind_limits(bounds) {
        Ok(limits) => limits,
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return Outcome::Refused(Refusal::Compose(format!(
                "the serve runtime could not be built: {error}"
            )))
        }
    };
    runtime.block_on(run_host(
        bounds,
        requested,
        receipt_root,
        limits,
        bind_limits,
    ))
}

/// Bind, compose, publish readiness, serve until a trigger fires, and report.
///
/// The order is the contract. Readiness is durable *before* the host is left to
/// run, so a supervisor that finds no receipt knows the host never came up; and
/// the disposition is the host's own, taken from the terminal receipt `stop`
/// returns, so this binary never renders an exit it merely expected.
async fn run_host(
    bounds: &ServeBounds,
    requested: SocketAddr,
    receipt_root: &Path,
    limits: ApiHostLimits,
    bind_limits: BindLimits,
) -> Outcome {
    let listener = match bind_listener(requested, bind_limits).await {
        Ok(listener) => listener,
        Err(text) => return Outcome::Refused(Refusal::Bind(text)),
    };
    let service = match production_service(&limits) {
        Ok(service) => service,
        Err(text) => return Outcome::Refused(Refusal::Compose(text)),
    };
    let prepared = match PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return Outcome::Refused(Refusal::Compose(error.to_string())),
    };
    let host = prepared.start();
    if let Err(refusal) = publish_ready(bounds, requested, host.ready(), receipt_root) {
        // The host is stopped before the refusal is rendered: a serve that
        // cannot attest it is up must not be left listening, and dropping the
        // owner would abort the root task without a receipt.
        let _ = host.stop().await;
        return Outcome::Refused(refusal);
    }
    let trigger = await_stop(bounds.serve_window).await;
    let exit = host.stop().await;
    Outcome::Ok(Success::Served {
        exit: receipt::render_exit(&exit),
        trigger,
    })
}

/// The run window raced against a real operator interrupt.
///
/// Both arms are real product paths. The window bounds an unattended run; the
/// interrupt is how a person stops a daemon, and it must win when it arrives
/// first. `ctrl_c` is reached through the `signal` feature already in this
/// crate's frozen closure, so racing it costs no dependency edge.
///
/// A failed signal registration is reported as the window rather than as an
/// interrupt: claiming an operator acted when the process could not even listen
/// for them would be the same class of false claim as a hardcoded exit.
async fn await_stop(window: ServeWindow) -> StopTrigger {
    tokio::select! {
        () = tokio::time::sleep(window.as_duration()) => StopTrigger::WindowElapsed,
        result = tokio::signal::ctrl_c() => match result {
            Ok(()) => StopTrigger::OperatorInterrupt,
            Err(_) => StopTrigger::WindowElapsed,
        },
    }
}

/// Write the ready receipt where the settled custody root says it may land.
///
/// The address is the one the operating system actually assigned, read from the
/// host's own readiness rather than from the plan: with an ephemeral `:0` bind
/// the requested address names no port anyone can connect to, and a receipt that
/// echoed it would be a false claim of the same family as a hardcoded exit.
fn publish_ready(
    bounds: &ServeBounds,
    requested: SocketAddr,
    ready: &ApiHostReady,
    receipt_root: &Path,
) -> Result<(), Refusal> {
    let receipt = ServeReceipt::from_bounds(bounds, ready.observed_address());
    match custody::persist_ready_receipt(receipt_root, &receipt.to_json("serve")) {
        Ok(path) => {
            // Prose, on the prose stream. stdout still carries exactly one JSON
            // object and it is the terminal receipt; this line is for the human
            // watching a terminal, and the durable artifact is for everyone else.
            let _ = writeln!(
                io::stderr(),
                "serve requested {requested}, ready on {} with a {} ms window; \
                 ready receipt at {}",
                ready.observed_address(),
                bounds.serve_window.milliseconds(),
                path.display()
            );
            Ok(())
        }
        Err(refusal) => Err(Refusal::Custody(refusal)),
    }
}

/// The accepted listener, through the accepted location crate.
async fn bind_listener(requested: SocketAddr, limits: BindLimits) -> Result<BoundListener, String> {
    let port = match std::num::NonZeroU16::new(requested.port()) {
        Some(port) => BindPort::Fixed(port),
        None => BindPort::Ephemeral,
    };
    let plan = ListenerBindPlan::new(BindHost::Ip(requested.ip()), port, limits);
    let binder = ListenerBinder::new(Arc::new(LiteralAddressResolver));
    binder
        .bind(&plan)
        .await
        .map_err(|error| format!("binding {requested} failed: {error}"))
}

/// The production service: an authenticated boundary over an empty router.
///
/// Every value here is a structural constant of E1a rather than an operator
/// choice, which is why none of them is a command-line bound. The directory is
/// empty by construction — `configure_empty` takes no authorities, which is what
/// makes this composition lawful without a `grapt-store` edge — and the router
/// is sealed with nothing registered, because this cut serves no product route,
/// not even `/health`. One mapping and one module are the smallest admissible
/// bounds over collections that are empty and stay empty.
fn production_service(limits: &ApiHostLimits) -> Result<AuthenticatedApiService, String> {
    let Ok(max_mappings) = MaxPrincipalMappings::try_new(1) else {
        return Err("the empty principal directory's bound of one was refused".to_owned());
    };
    let directory = SeatPrincipalDirectory::configure_empty(max_mappings);
    let Ok(policy) = AuthenticationPolicyVersion::try_new("auth-policy-v1") else {
        return Err("the authentication policy version was refused".to_owned());
    };
    let boundary = Arc::new(SeatTrustBoundary::new(policy, directory));
    let Ok(max_modules) = MaxRouterModules::try_new(1) else {
        return Err("the empty router's module bound of one was refused".to_owned());
    };
    Ok(AuthenticatedApiService::new(
        ApiRouterBuilder::new(max_modules).seal(),
        boundary,
        limits.max_request_body_bytes,
    ))
}

/// Every host bound, validated by the crate that owns it.
///
/// A bound the operator supplied and the owning crate refused is an *argument*
/// refusal naming that flag, not a composition failure: the value came from the
/// command line and the caller is the one who can fix it.
fn host_limits(bounds: &ServeBounds) -> Result<ApiHostLimits, ArgError> {
    let invalid = |flag: &'static str, value: u64| ArgError::Invalid {
        flag,
        value: value.to_string(),
    };
    let Ok(max_connections) = MaxConnections::try_new(bounds.max_connections) else {
        return Err(invalid("--max-connections", bounds.max_connections as u64));
    };
    let Ok(per_connection) = u32::try_from(bounds.max_requests_per_connection)
        .map_err(|_| ())
        .and_then(|value| MaxRequestsPerConnection::try_new(value).map_err(|_| ()))
    else {
        return Err(invalid(
            "--max-requests-per-connection",
            bounds.max_requests_per_connection as u64,
        ));
    };
    let Ok(max_inflight_requests) = MaxInflightRequests::try_new(bounds.max_inflight) else {
        return Err(invalid("--max-inflight", bounds.max_inflight as u64));
    };
    let Ok(max_request_head_bytes) = MaxRequestHeadBytes::try_new(bounds.max_request_head_bytes)
    else {
        return Err(invalid(
            "--max-request-head-bytes",
            bounds.max_request_head_bytes as u64,
        ));
    };
    let Ok(max_headers) = MaxHeaders::try_new(bounds.max_headers) else {
        return Err(invalid("--max-headers", bounds.max_headers as u64));
    };
    let Ok(max_request_body_bytes) = MaxRequestBodyBytes::try_new(bounds.max_request_body_bytes)
    else {
        return Err(invalid(
            "--max-request-body-bytes",
            bounds.max_request_body_bytes as u64,
        ));
    };
    let Ok(header_read_allowance) = HeaderReadAllowance::try_new(std::time::Duration::from_millis(
        bounds.header_read_allowance_ms,
    )) else {
        return Err(invalid(
            "--header-read-allowance-ms",
            bounds.header_read_allowance_ms,
        ));
    };
    let Ok(shutdown_drain_allowance) = ShutdownDrainAllowance::try_new(
        std::time::Duration::from_millis(bounds.shutdown_drain_allowance_ms),
    ) else {
        return Err(invalid(
            "--shutdown-drain-allowance-ms",
            bounds.shutdown_drain_allowance_ms,
        ));
    };
    // The engine supplies the same production write bound the product's host
    // composition does, and no flag narrows or widens it. The constant is
    // nonzero, so this branch is unreachable; it is REPRESENTED rather than
    // asserted away, because this crate denies `expect`.
    let Ok(stream_write_allowance) =
        StreamWriteAllowance::try_new(std::time::Duration::from_secs(30))
    else {
        return Err(ArgError::Invalid {
            flag: "stream_write_allowance",
            value: "the production write bound was refused".to_owned(),
        });
    };
    Ok(ApiHostLimits {
        max_connections,
        max_requests_per_connection: per_connection,
        max_inflight_requests,
        max_inflight_streams: grapt_api::MaxInflightStreams::disabled(),
        max_request_head_bytes,
        max_headers,
        max_request_body_bytes,
        header_read_allowance,
        shutdown_drain_allowance,
        stream_write_allowance,
    })
}

/// The two bind bounds, validated by the location crate that owns them.
fn bind_limits(bounds: &ServeBounds) -> Result<BindLimits, ArgError> {
    let Ok(addresses) = MaxResolvedAddresses::new(bounds.max_resolved_addresses) else {
        return Err(ArgError::Invalid {
            flag: "--max-resolved-addresses",
            value: bounds.max_resolved_addresses.to_string(),
        });
    };
    let Ok(attempts) = MaxBindAttempts::new(bounds.bind_attempts) else {
        return Err(ArgError::Invalid {
            flag: "--bind-attempts",
            value: bounds.bind_attempts.to_string(),
        });
    };
    Ok(BindLimits::new(addresses, attempts))
}

/// `service ensure|stop|status`.
fn run_service(args: &[String]) -> Outcome {
    let Some(sub) = args.first() else {
        return Outcome::Refused(Refusal::Argument(ArgError::UnknownVerb(String::new())));
    };
    let rest = &args[1..];
    if let Err(error) = reject_unknown(rest, SERVICE_FLAGS) {
        return Outcome::Refused(Refusal::Argument(error));
    }
    let registration = match required(rest, "--registration") {
        Ok(value) => PathBuf::from(value),
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let attempts = match required_number(rest, "--lock-attempts") {
        Ok(value) => value,
        Err(error) => return Outcome::Refused(Refusal::Argument(error)),
    };
    let Some(policy) = u32::try_from(attempts)
        .ok()
        .and_then(grapt_service::LockAttemptPolicy::new)
    else {
        return Outcome::Refused(Refusal::Argument(ArgError::Invalid {
            flag: "--lock-attempts",
            value: attempts.to_string(),
        }));
    };
    if let Err(refusal) = settle_receipt_root(rest) {
        return Outcome::Refused(refusal);
    }
    match sub.as_str() {
        "status" => match service::run_status(&registration, policy) {
            Ok(outcome) => Outcome::Ok(Success::Service(outcome)),
            Err(refusal) => Outcome::Refused(Refusal::Service(refusal)),
        },
        "ensure" | "stop" => Outcome::Refused(Refusal::Service(service::ServiceRefusal::Control {
            reason: format!(
                "service {sub} requires an elected controller authority and an attributed probe, \
                 neither of which E1a issues"
            ),
        })),
        other => Outcome::Refused(Refusal::Argument(ArgError::UnknownVerb(other.to_owned()))),
    }
}
