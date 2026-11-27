//! A busy seat: the real TUI in a real terminal against the real service while
//! one session streams small deltas fast. The journey types, submits and
//! interrupts throughout, cuts the live connections, slows the provider and
//! restarts the service, then requires that input stayed accepted, every submit
//! landed exactly once, and the view resumed after each disruption.
//!
//! Echo is byte presence: each probe is a Greek letter that no provider text
//! carries, typed and then erased before the cycle submits, so a transcript
//! redraw can never echo a probe early. Echo percentiles are reported as data;
//! the gate holds only a ceiling that survives a loaded build host.
use super::*;
use tokio::time::Instant;

#[path = "tui_busy_seat/fresh_binary.rs"]
mod fresh_binary;
#[path = "tui_busy_seat/proxy.rs"]
mod proxy;
#[path = "tui_busy_seat/screen.rs"]
mod screen;
#[path = "tui_busy_seat/terminal.rs"]
mod terminal;

#[path = "tui_busy_seat/resume.rs"]
mod resume;

use proxy::Relay;
use terminal::Terminal;

static TICKS: AtomicUsize = AtomicUsize::new(1);
const PROBES: [&str; 24] = [
    "α", "β", "γ", "δ", "ε", "ζ", "η", "θ", "ι", "κ", "λ", "μ", "ν", "ξ", "ο", "π", "ρ", "σ", "τ",
    "υ", "φ", "χ", "ψ", "ω",
];
const ECHO_P95: Duration = Duration::from_millis(100);
const ECHO_CEILING: Duration = Duration::from_secs(1);
const RECOVERY: Duration = Duration::from_secs(3);
/// A restart reattaches through the client's reconnect backoff: its longest
/// wait between attempts, plus the attachment and the first streamed delta.
const REATTACH: Duration =
    grapt_client::client::RECONNECT_RETRY_MAX.saturating_add(Duration::from_secs(6));
const MARKER: &str = "busy-seat-cycle-";
const DOUBLE: &str = "busy-seat-double-";
/// Stream labels: ordinary streams, and the long stream a gap cuts.
const TICK: &str = "tick";
const LONG: &str = "long";

#[derive(Clone, Copy, Debug)]
enum Disruption {
    None,
    Gap,
    Slow,
    Interrupt,
    Restart,
    /// A second Enter while the first submission is still in flight.
    DoubleSubmit,
}

struct Load {
    label: &'static str,
    cycles: usize,
    probes: usize,
    keystroke: Duration,
    disruptions: &'static [(usize, Disruption)],
}

#[test]
fn a_busy_seat_keeps_input_and_recovers_from_gaps_stalls_and_a_restart() {
    busy_seat(&Load {
        label: "tui-busy-seat",
        cycles: 10,
        probes: 16,
        keystroke: Duration::from_millis(120),
        disruptions: &[
            (2, Disruption::Gap),
            (3, Disruption::Interrupt),
            (4, Disruption::DoubleSubmit),
            (5, Disruption::Slow),
            (6, Disruption::Gap),
            (8, Disruption::Restart),
        ],
    });
}

#[test]
fn a_busy_seat_soak_keeps_input_and_recovers_for_ten_minutes() {
    const SOAK: &[(usize, Disruption)] = &[
        (10, Disruption::Gap),
        (20, Disruption::DoubleSubmit),
        (25, Disruption::Interrupt),
        (40, Disruption::Slow),
        (60, Disruption::Gap),
        (80, Disruption::Restart),
        (110, Disruption::Gap),
        (140, Disruption::Slow),
        (170, Disruption::Restart),
        (190, Disruption::Interrupt),
        (200, Disruption::Gap),
    ];
    busy_seat(&Load {
        label: "tui-busy-seat-soak",
        cycles: 200,
        probes: 20,
        keystroke: Duration::from_millis(120),
        disruptions: SOAK,
    });
}

/// A streaming answer for every cycle; compaction summaries get a concise
/// factual objective instead of matching a quoted cycle marker in history.
fn busy_script(body: &str) -> Script {
    if body.contains("Summarize the supplied conversation data") {
        return Script::SuccessText(
            "Objective: continue the busy-seat conversation.\nState: prior cycles settled cleanly.\nNext: answer the latest busy-seat cycle.",
        );
    }
    let Some(at) = body.rfind(MARKER) else {
        return Script::Success;
    };
    let marker = body[at..].split('"').next().unwrap_or("");
    let (label, burst, deltas, interval) = match Stream::of(marker) {
        Stream::Slow => (TICK, 0, 4, Duration::from_secs(1)),
        // A gap needs its own stream running when the connections are cut.
        Stream::Long => (LONG, 400, 1_000, Duration::from_millis(5)),
        // The opening burst outruns a terminal that draws slower than it reads.
        Stream::Normal => (TICK, 400, 300, Duration::from_millis(5)),
    };
    Script::Paced {
        label,
        burst,
        deltas,
        interval,
        ticks: &TICKS,
        hold: None,
        breaks: false,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stream {
    Normal,
    Slow,
    Long,
}

impl Stream {
    fn of(marker: &str) -> Self {
        if marker.contains("-slow") {
            Self::Slow
        } else if marker.contains("-long") {
            Self::Long
        } else {
            Self::Normal
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::Slow => "-slow",
            Self::Long => "-long",
        }
    }
}

fn busy_seat(load: &Load) {
    let _journey = process_journey_custody();
    // This journey cuts live connections and restarts the service on purpose, so
    // a request its own cut ends mid-body is expected, not refused input. Every
    // other refusal still fails the fixture.
    let provider = ProviderFixture::start_tolerating_cuts(vec![
        Script::ForRequest(busy_script);
        4 * load.cycles + 16
    ]);
    let mut run = Run::start(load.label, provider.address);
    let mut echoes = Vec::new();
    let began = Instant::now();
    run.submitted.push(submit(&run.tui, 0, Stream::Normal));
    require_fresh_tick(&run.tui, 0, Duration::from_secs(10), "first stream");
    for cycle in 1..=load.cycles {
        let disruption = load
            .disruptions
            .iter()
            .find(|(at, _)| *at == cycle)
            .map_or(Disruption::None, |(_, disruption)| *disruption);
        let seen = latest_tick(&run.tui);
        eprintln!(
            "{}: cycle {cycle} {disruption:?} at {:?}, newest tick {seen}, wall_ms={}",
            load.label,
            began.elapsed(),
            wall_ms()
        );
        match disruption {
            Disruption::Gap => run.gap(cycle, seen),
            Disruption::Interrupt => run.interrupt(cycle, seen),
            Disruption::Restart => run.restart(cycle),
            Disruption::Slow | Disruption::DoubleSubmit | Disruption::None => {}
        }
        echoes.extend(probe(&run.tui, load.probes, load.keystroke, cycle));
        match disruption {
            Disruption::Gap => {}
            Disruption::Interrupt => run.after_interrupt(cycle),
            _ if resend_held(&run.tui) => {}
            Disruption::Slow => run.submitted.push(submit(&run.tui, cycle, Stream::Slow)),
            Disruption::DoubleSubmit => run.double_submit(cycle),
            _ => run.submitted.push(submit(&run.tui, cycle, Stream::Normal)),
        }
    }
    let landed = settle_submits(
        run.address,
        &run.session,
        &run.submitted,
        Duration::from_secs(30),
    );
    report(load.label, &echoes, &run.recoveries);
    for (marker, count) in run.submitted.iter().zip(&landed) {
        assert_eq!(
            *count,
            1,
            "{marker} landed {count} times; one Enter must send exactly once:\n{}",
            run.tui.screen()
        );
    }
    assert_echoes(&echoes);
}

/// A loaded build host stretches single samples, so the gate holds the
/// distribution to a budget and only the slowest sample to a loose ceiling.
fn assert_echoes(echoes: &[Duration]) {
    let mut sorted = echoes.to_vec();
    sorted.sort_unstable();
    let p95 = sorted
        .get(sorted.len().saturating_sub(1) * 95 / 100)
        .copied()
        .unwrap_or_default();
    let slowest = sorted.last().copied().unwrap_or_default();
    assert!(
        p95 <= ECHO_P95,
        "echo p95 {p95:?} exceeded {ECHO_P95:?} under a busy stream"
    );
    assert!(
        slowest <= ECHO_CEILING,
        "slowest echo {slowest:?} exceeded {ECHO_CEILING:?} under a busy stream"
    );
}

/// Removes the fixture root however the journey ends.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        if thread::panicking() {
            // A second panic would abort the harness and hide the first failure.
            if let Err(error) = fs::remove_dir_all(&self.0) {
                eprintln!("busy-seat fixture {} kept: {error}", self.0.display());
            }
        } else {
            process_fixture::cleanup(&self.0);
        }
    }
}

/// One busy seat. Fields drop in order, so the TUI, the relay and the service
/// have all ended before the root is removed, on success and on panic alike.
struct Run {
    tui: Terminal,
    relay: Relay,
    /// Empty only between a killed service and its successor.
    service: Option<process_fixture::ManagedChild>,
    provider: SocketAddr,
    address: SocketAddr,
    session: String,
    mirror: PathBuf,
    submitted: Vec<String>,
    recoveries: Vec<(&'static str, Duration)>,
    root: Root,
}

impl Run {
    fn start(label: &str, provider: SocketAddr) -> Self {
        let root = Root(root(label));
        prepare(&root.0, 1_800_000);
        prepare_model_inventory(&root.0, 400_000);
        let mut service = spawn(&root.0, provider);
        let address = await_ready(&root.0, &mut service);
        let session = seat_session(address, &root.0);
        let relay = Relay::start(address);
        let mirror = root.0.join("tui-profile");
        mirror_profile(&root.0, &mirror, relay.address);
        let home = root.0.join("tui-home");
        fs::create_dir_all(&home).unwrap_or_else(|error| panic!("TUI home: {error}"));
        let tui = Terminal::spawn(
            &tui_binary(),
            &["--service", path_text(&mirror), "--session", &session],
            &[
                ("HOME", &home),
                ("GRAPT_TEST_HOME", &home),
                ("GRAPT_CONFIG_DIR", &home),
            ],
            &root.0.join("terminal.log"),
        );
        // Typing before the TUI owns the terminal would reach the line
        // discipline, not the TUI. The session's model on screen means the
        // catalog resolved; the first echo then proves raw input reaches it.
        if tui
            .wait_until(Duration::from_secs(30), |screen| {
                screen.contains("gpt-live")
            })
            .is_none()
        {
            panic!("TUI never showed the seat's model:\n{}", tui.screen());
        }
        tui.send(PROBES[0].as_bytes());
        if tui
            .wait_until(Duration::from_secs(30), |screen| screen.contains(PROBES[0]))
            .is_none()
        {
            panic!("TUI never echoed its first key:\n{}", tui.screen());
        }
        erase(&tui, 1, 0);
        Self {
            tui,
            relay,
            service: Some(service),
            provider,
            address,
            session,
            mirror,
            submitted: Vec::new(),
            recoveries: Vec::new(),
            root,
        }
    }

    /// Cuts every live connection while a long stream runs; the view must resume.
    fn gap(&mut self, cycle: usize, seen: usize) {
        // The previous cycle's stream running proves its submit settled.
        require_fresh_tick(&self.tui, seen, Duration::from_secs(10), "previous stream");
        let before = latest(&self.tui, LONG);
        self.submitted.push(submit(&self.tui, cycle, Stream::Long));
        // Measure recovery only: the submission settles before the cut.
        confirmed(&self.tui, cycle);
        let long_stream = Duration::from_secs(20);
        require_fresh(
            &self.tui,
            &[LONG],
            before,
            long_stream,
            "long stream before a cut",
        );
        let seen = latest(&self.tui, LONG);
        // Taken before the cut: a reconnect must not count toward recovery.
        let subscribed = self.relay.subscriptions();
        let cut = self.relay.cut();
        assert!(
            cut > 0,
            "cycle {cycle}: the TUI held no live connection to cut"
        );
        // Recovery is measured from the resubscribe the reconnect produces, not
        // from the cut. The client backs off up to RECONNECT_RETRY_MAX before
        // it reconnects, so a window timed from the cut is measuring that
        // backoff against a smaller allowance and fails whenever the attempt
        // lands late in it, which is what the nightly runs did.
        let reconnect = PollDeadline::within(REATTACH);
        while self.relay.subscriptions() == subscribed {
            assert!(
                reconnect.pending(),
                "cycle {cycle}: the TUI never subscribed again after the cut"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        require_fresh(&self.tui, &[LONG], seen, RECOVERY, "live view after a cut");
        self.recoveries.push(("gap", started.elapsed()));
    }

    /// Interrupts a stream that is visibly running and lets it stop, so the
    /// interrupt can only reach the work it was aimed at.
    fn interrupt(&mut self, cycle: usize, seen: usize) {
        self.submitted.push(submit(&self.tui, cycle, Stream::Long));
        let long_stream = Duration::from_secs(20);
        require_fresh(&self.tui, &[LONG], seen, long_stream, "stream to interrupt");
        self.tui.send(b"\x03");
        let wait = PollDeadline::within(Duration::from_secs(10));
        loop {
            let response = json(self.address, "GET", "/api/session/active", "");
            match session_settled_in_active_response(&response, &self.session) {
                Ok(true) => return,
                Ok(false) => {}
                Err(reason) => panic!("cycle {cycle}: {reason}"),
            }
            assert!(
                wait.pending(),
                "cycle {cycle}: the interrupted stream never settled"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The next submit after an interrupt must stream.
    fn after_interrupt(&mut self, cycle: usize) {
        let again = format!("{MARKER}{cycle:03}-again-end");
        self.submitted
            .push(type_and_enter(&self.tui, &again, cycle));
        let started = Instant::now();
        let seen = latest_tick(&self.tui);
        require_fresh_tick(&self.tui, seen, RECOVERY, "stream after an interrupt");
        self.recoveries.push(("interrupt", started.elapsed()));
    }

    /// Restarts the service on the same profile and republishes its endpoint.
    fn restart_service(&mut self) {
        let previous = ready_pid(&self.root.0);
        self.relay.suspend();
        // The old service ends before its successor opens the same profile.
        self.service = None;
        let service = self.service.insert(spawn(&self.root.0, self.provider));
        // A killed service leaves its receipt behind, so readiness is the
        // successor's own receipt, and its endpoint is the new one.
        let wait = PollDeadline::within(Duration::from_secs(30));
        while ready_pid(&self.root.0) == previous {
            assert!(
                wait.pending(),
                "the restarted service never published its receipt"
            );
            thread::sleep(Duration::from_millis(20));
        }
        self.address = await_ready(&self.root.0, service);
        self.relay.resume(self.address);
        mirror_profile(&self.root.0, &self.mirror, self.relay.address);
    }

    /// Restarts the service on the same profile; the TUI must reattach.
    fn restart(&mut self, cycle: usize) {
        self.restart_service();
        // The service notice can outlive recovery, so reattachment is measured
        // by behaviour: the held submit, resent the way a user would, streams.
        let seen = latest_tick(&self.tui);
        let started = Instant::now();
        loop {
            resend_held(&self.tui);
            let fresh = |screen: &str| ticks_in(screen, TICK).any(|tick| tick > seen);
            if self.tui.wait_until(Duration::from_secs(1), fresh).is_some() {
                break;
            }
            if started.elapsed() > REATTACH {
                panic!(
                    "cycle {cycle}: no stream {REATTACH:?} after the service was ready:\n{}",
                    self.tui.screen()
                );
            }
        }
        self.recoveries
            .push(("reattach after restart", started.elapsed()));
    }

    /// Two Enters while the service answers slowly: both must land, each once.
    fn double_submit(&mut self, cycle: usize) {
        // Added response latency keeps the first submission unconfirmed while
        // the second is typed, so the step does not depend on a race. It stays
        // below what would stall the live view into a reconnect.
        self.relay.delay(Duration::from_millis(150));
        let first = submit(&self.tui, cycle, Stream::Normal);
        let second = type_and_enter(&self.tui, &format!("{DOUBLE}{cycle:03}-end"), cycle);
        let pair = [first, second];
        let wait = PollDeadline::within(Duration::from_secs(30));
        let landed = loop {
            // A refusal while reconnecting holds the text; the user resends it.
            resend_held(&self.tui);
            let landed = settle_submits(self.address, &self.session, &pair, Duration::ZERO);
            if landed.iter().all(|count| *count > 0) || !wait.pending() {
                break landed;
            }
            thread::sleep(Duration::from_millis(50));
        };
        self.relay.delay(Duration::ZERO);
        assert_eq!(
            landed,
            [1, 1],
            "cycle {cycle}: two Enters in quick succession sent {landed:?} of {pair:?}:\n{}",
            self.tui.screen()
        );
        self.submitted.extend(pair);
    }
}

/// Types `count` distinct probes, timing each echo, then erases them.
fn probe(tui: &Terminal, count: usize, keystroke: Duration, cycle: usize) -> Vec<Duration> {
    let mut echoes = Vec::with_capacity(count);
    for (index, probe) in PROBES.iter().take(count).enumerate() {
        assert!(
            !tui.screen().contains(probe),
            "cycle {cycle}: probe {probe} was on screen before it was typed:\n{}",
            tui.screen()
        );
        let sent_ms = wall_ms();
        let sent = Instant::now();
        tui.send(probe.as_bytes());
        let Some(seen) = tui.wait_until(Duration::from_secs(5), |screen| screen.contains(probe))
        else {
            panic!("cycle {cycle}: key {probe} never echoed:\n{}", tui.screen());
        };
        let echo = seen - sent;
        echoes.push(echo);
        eprintln!(
            "tui-busy-seat: probe {}",
            serde_json::json!({"cycle":cycle,"probe":index + 1,
                "sent_ms":sent_ms,"observed_ms":wall_ms(),"echo_us":echo.as_micros()}),
        );
        thread::sleep(keystroke);
    }
    erase(tui, count, cycle);
    echoes
}

/// Wall time correlates measured input with independently captured service
/// events; latency itself remains measured by the monotonic test clock.
fn wall_ms() -> i64 {
    grapt_types::ClockAuthority::now(&grapt_store::SystemClock).as_millis()
}

/// Erases the typed probes and waits until none is left on screen.
fn erase(tui: &Terminal, count: usize, cycle: usize) {
    tui.send(&vec![0x7f; count]);
    let gone = |screen: &str| PROBES.iter().all(|probe| !screen.contains(probe));
    if tui.wait_until(Duration::from_secs(5), gone).is_none() {
        panic!(
            "cycle {cycle}: erased probes stayed on screen:\n{}",
            tui.screen()
        );
    }
}

/// Submits this cycle's marker and returns it.
fn submit(tui: &Terminal, cycle: usize, stream: Stream) -> String {
    type_and_enter(
        tui,
        &format!("{MARKER}{cycle:03}{}-end", stream.suffix()),
        cycle,
    )
}

/// Types `marker` and presses Enter once it shows, returning the marker.
fn type_and_enter(tui: &Terminal, marker: &str, cycle: usize) -> String {
    tui.send(marker.as_bytes());
    if tui
        .wait_until(Duration::from_secs(5), |screen| screen.contains(marker))
        .is_none()
    {
        panic!(
            "cycle {cycle}: the composer never showed its submit:\n{}",
            tui.screen()
        );
    }
    tui.send(b"\r");
    marker.to_owned()
}

/// A refused submit waits, held; the user sends it again with Enter on an
/// empty draft. True when this cycle did that instead of a new submit.
fn resend_held(tui: &Terminal) -> bool {
    if !tui.screen().contains("held · Enter") {
        return false;
    }
    tui.send(b"\r");
    true
}

/// Waits until no submission is in flight or waiting behind one.
fn confirmed(tui: &Terminal, cycle: usize) {
    let settled =
        |screen: &str| !screen.contains("Sending · ") && !screen.contains("Waiting to send");
    if tui.wait_until(Duration::from_secs(10), settled).is_none() {
        panic!(
            "cycle {cycle}: a submission never settled:\n{}",
            tui.screen()
        );
    }
}

/// The newest delta number on screen now, of any stream.
fn latest_tick(tui: &Terminal) -> usize {
    latest(tui, TICK).max(latest(tui, LONG))
}

fn latest(tui: &Terminal, label: &str) -> usize {
    ticks_in(&tui.screen(), label).max().unwrap_or(0)
}

fn ticks_in<'a>(text: &'a str, label: &'a str) -> impl Iterator<Item = usize> + 'a {
    let prefix = label.len() + 1;
    text.match_indices(label)
        .filter(move |(at, _)| text[at + label.len()..].starts_with('-'))
        .filter_map(move |(at, _)| text.get(at + prefix..at + prefix + 6)?.parse().ok())
}

fn require_fresh_tick(tui: &Terminal, after: usize, within: Duration, what: &str) {
    require_fresh(tui, &[TICK, LONG], after, within, what);
}

fn require_fresh(tui: &Terminal, labels: &[&str], after: usize, within: Duration, what: &str) {
    let fresh = |screen: &str| {
        labels
            .iter()
            .any(|label| ticks_in(screen, label).any(|tick| tick > after))
    };
    if tui.wait_until(within, fresh).is_none() {
        panic!(
            "{what} showed no tick after {after} within {within:?}:\n{}",
            tui.screen()
        );
    }
}

/// Every user message of the session, read page by page.
fn user_messages(address: SocketAddr, session: &str) -> Vec<String> {
    let mut users = Vec::new();
    let mut path = format!("/api/session/{session}/message?order=asc&limit=200");
    for _ in 0..100 {
        let page = json(address, "GET", &path, "");
        users.extend(
            page["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["type"] == "user")
                .map(serde_json::Value::to_string),
        );
        let Some(cursor) = page["cursor"]["next"].as_str() else {
            return users;
        };
        path = format!("/api/session/{session}/message?limit=200&cursor={cursor}");
    }
    panic!("message history did not end within 100 pages");
}

/// Each submitted marker's count across the session's user messages.
fn settle_submits(
    address: SocketAddr,
    session: &str,
    markers: &[String],
    within: Duration,
) -> Vec<usize> {
    let wait = PollDeadline::within(within);
    loop {
        let users = user_messages(address, session);
        let counts: Vec<usize> = markers
            .iter()
            .map(|marker| {
                users
                    .iter()
                    .map(|message| message.matches(marker.as_str()).count())
                    .sum()
            })
            .collect();
        if counts.iter().all(|count| *count > 0) || !wait.pending() {
            return counts;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn report(label: &str, echoes: &[Duration], recoveries: &[(&str, Duration)]) {
    let mut sorted: Vec<u128> = echoes.iter().map(Duration::as_millis).collect();
    sorted.sort_unstable();
    let at = |share: usize| {
        sorted
            .get((sorted.len().saturating_sub(1)) * share / 100)
            .copied()
            .unwrap_or_default()
    };
    eprintln!(
        "{label}: echo ms p50={} p95={} max={} samples={}; recoveries {:?}",
        at(50),
        at(95),
        at(100),
        sorted.len(),
        recoveries
    );
}

/// A session with the agent, model and location a provisioned seat carries.
fn seat_session(address: SocketAddr, root: &Path) -> String {
    let body = serde_json::json!({
        "agent": "agt_live_headless1",
        "model": {"providerID": "openai", "id": "gpt-live"},
        "location": {"directory": path_text(root).replace('\\', "/")},
    });
    json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("seat session had no id"))
        .to_owned()
}

/// The process that published the profile's current readiness receipt.
fn ready_pid(root: &Path) -> Option<u64> {
    let ready = fs::read_to_string(root.join("serve-ready.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&ready).ok()?["pid"].as_u64()
}

/// A profile copy whose readiness names the relay, as the TUI's only route.
fn mirror_profile(root: &Path, mirror: &Path, relay: SocketAddr) {
    fs::create_dir_all(mirror).unwrap_or_else(|error| panic!("mirror profile: {error}"));
    let ready = fs::read_to_string(root.join("serve-ready.json"))
        .unwrap_or_else(|error| panic!("service readiness: {error}"));
    let mut ready: serde_json::Value =
        serde_json::from_str(&ready).unwrap_or_else(|error| panic!("readiness JSON: {error}"));
    ready["observed_endpoint"] = serde_json::Value::String(format!("http://{relay}"));
    fs::copy(
        root.join("basic.credential"),
        mirror.join("basic.credential"),
    )
    .unwrap_or_else(|error| panic!("mirror credential: {error}"));
    // Replace the receipt in one step so the TUI never reads half of one.
    let staged = mirror.join("serve-ready.json.next");
    fs::write(&staged, ready.to_string()).unwrap_or_else(|error| panic!("mirror receipt: {error}"));
    fs::rename(&staged, mirror.join("serve-ready.json"))
        .unwrap_or_else(|error| panic!("mirror receipt publish: {error}"));
}

/// The TUI is a sibling binary, built with the product's own target directory.
pub(crate) fn tui_binary() -> PathBuf {
    let tui = shipped_cli::product_binary()
        .with_file_name("grapt-tui")
        .with_extension(std::env::consts::EXE_EXTENSION);
    assert!(
        tui.is_file(),
        "TUI prerequisite missing: {}; run `cargo build -p grapt-tui` with the same target \
         directory and profile",
        tui.display()
    );
    fresh_binary::require_fresh_build(&tui);
    tui
}

fn path_text(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| panic!("fixture path is not UTF-8: {}", path.display()))
}

/// Held after its burst while the mid-stream cut is made.
static CUT_HELD: AtomicBool = AtomicBool::new(false);
/// Marked requests the provider served for the mid-stream cut.
static CUT_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A long answer, for the marked turn only, that waits after its burst until
/// the cut has been made; any other provider call simply succeeds.
fn cut_script(body: &str) -> Script {
    if !body.contains(MARKER) {
        return Script::Success;
    }
    CUT_CALLS.fetch_add(1, Ordering::SeqCst);
    Script::Paced {
        label: LONG,
        burst: 400,
        deltas: 1_000,
        interval: Duration::from_millis(5),
        ticks: &TICKS,
        hold: Some(&CUT_HELD),
        // Held text must be on screen before the cut, so it ends in lines.
        breaks: true,
    }
}

/// Releases the held stream however the test ends.
struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CUT_HELD.store(false, Ordering::SeqCst);
    }
}

/// A connection cut while an answer is still streaming must show the rest of
/// that answer within the recovery budget: the re-read the cut causes catches
/// the answer mid-way, and the deltas that follow continue it on screen.
#[test]
fn a_cut_mid_stream_shows_the_rest_of_the_answer_within_the_budget() {
    let _journey = process_journey_custody();
    let release = Release;
    CUT_HELD.store(true, Ordering::SeqCst);
    CUT_CALLS.store(0, Ordering::SeqCst);
    let provider = ProviderFixture::start(vec![Script::ForRequest(cut_script); 8]);
    let run = Run::start("tui-mid-stream-cut", provider.address);
    let first = TICKS.load(Ordering::SeqCst);
    submit(&run.tui, 0, Stream::Long);
    // The whole burst is on screen, and the stream is held after it.
    let burst = run.tui.wait_until(Duration::from_secs(20), |screen| {
        ticks_in(screen, LONG).any(|tick| tick >= first + 399)
    });
    assert!(
        burst.is_some(),
        "the burst never reached the screen:\n{}",
        run.tui.screen()
    );
    let seen = latest(&run.tui, LONG);
    // The baseline is taken before the cut: a fast reconnect must not count
    // toward it, or the wait below would look for a second one.
    let subscribed = run.relay.subscriptions();
    let cut = run.relay.cut();
    assert!(cut > 0, "the TUI held no live connection to cut");
    // Released only once the TUI has subscribed to the stream again, so the
    // rest of the answer is produced after the cut, while the client recovers.
    let reconnect = PollDeadline::within(Duration::from_secs(10));
    while run.relay.subscriptions() == subscribed {
        assert!(
            reconnect.pending(),
            "the TUI never subscribed again after the cut"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(release);
    // Well past the cut, and still advancing: the answer streams on.
    require_fresh(
        &run.tui,
        &[LONG],
        seen + 50,
        RECOVERY,
        "the answer after a mid-stream cut",
    );
    let advanced = latest(&run.tui, LONG);
    require_fresh(
        &run.tui,
        &[LONG],
        advanced,
        RECOVERY,
        "the answer still streaming after a mid-stream cut",
    );
    // The fresh ticks are the same answer continuing, not a retried request.
    assert_eq!(
        CUT_CALLS.load(Ordering::SeqCst),
        1,
        "the marked turn was requested again"
    );
}

/// Held mid-line while the stalled-line case looks at the screen.
static STALLED_HELD: AtomicBool = AtomicBool::new(false);

/// Deltas with no line break, then a pause: a model stopping mid-line.
fn stalled_script(_body: &str) -> Script {
    Script::Paced {
        label: LONG,
        burst: 25,
        deltas: 5,
        interval: Duration::from_millis(5),
        ticks: &TICKS,
        hold: Some(&STALLED_HELD),
        // A model stopping mid-line: no line breaks.
        breaks: false,
    }
}

/// A model that pauses mid-line must not look stuck: streamed text is drawn a
/// line at a time, and what it already sent of an unfinished line must show
/// within a second of the pause.
#[test]
fn a_stream_paused_mid_line_shows_what_it_sent_within_a_second() {
    let _journey = process_journey_custody();
    STALLED_HELD.store(true, Ordering::SeqCst);
    let provider = ProviderFixture::start(vec![Script::ForRequest(stalled_script); 8]);
    let run = Run::start("tui-stalled-line", provider.address);
    let first = TICKS.load(Ordering::SeqCst);
    submit(&run.tui, 0, Stream::Normal);
    // The provider has sent its burst and is holding: the pause has begun.
    let sent = PollDeadline::within(Duration::from_secs(20));
    while TICKS.load(Ordering::SeqCst) < first + 25 {
        assert!(sent.pending(), "the provider never streamed its burst");
        thread::sleep(Duration::from_millis(5));
    }
    let shown = run.tui.wait_until(Duration::from_secs(1), |screen| {
        ticks_in(screen, LONG).any(|tick| tick >= first)
    });
    STALLED_HELD.store(false, Ordering::SeqCst);
    assert!(
        shown.is_some(),
        "a stream paused mid-line showed nothing of its line within 1 s:\n{}",
        run.tui.screen()
    );

    let _completed = run
        .tui
        .wait_until(Duration::from_secs(10), |screen| {
            ticks_in(screen, LONG).any(|tick| tick == first + 29)
        })
        .unwrap_or_else(|| panic!("the completed line never replaced its provisional text"));
    let final_screen = run.tui.screen();
    let ticks = ticks_in(&final_screen, LONG)
        .filter(|tick| (first..first + 30).contains(tick))
        .collect::<Vec<_>>();
    let unique = ticks
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 30, "the completed line lost streamed text");
    assert_eq!(
        ticks.len(),
        unique.len(),
        "provisional text duplicated after completion"
    );
}

fn session_settled_in_active_response(
    response: &serde_json::Value,
    session: &str,
) -> Result<bool, String> {
    let active = response
        .get("data")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("/api/session/active returned non-object data: {response}"))?;
    Ok(!active.contains_key(session))
}

#[test]
fn active_session_settlement_rejects_non_object_data() {
    for invalid in [
        serde_json::json!({ "data": [] }),
        serde_json::json!({ "data": null }),
        serde_json::json!({ "data": "idle" }),
        serde_json::json!({}),
    ] {
        assert!(
            session_settled_in_active_response(&invalid, "ses-1").is_err(),
            "non-object /api/session/active payload must not report settled: {invalid}"
        );
    }
    assert_eq!(
        session_settled_in_active_response(
            &serde_json::json!({ "data": { "ses-1": {} } }),
            "ses-1"
        ),
        Ok(false)
    );
    assert_eq!(
        session_settled_in_active_response(&serde_json::json!({ "data": {} }), "ses-1"),
        Ok(true)
    );
}
