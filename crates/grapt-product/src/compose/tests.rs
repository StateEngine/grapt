use super::*;
use crate::compose_host::{
    bind_loopback, drain_host, host_limits, product_service, registration_was_released,
};
use grapt_service::StopOutcome;
use grapt_store::{apply_migrations, EventStore, MigrationOutcome, ProductionMigrations};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

static SOCKET_WITNESS_CUSTODY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ready_service_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "grapt-product-compose-{}-{label}",
        std::process::id()
    ));
    if root.exists() {
        fs::remove_dir_all(&root)
            .unwrap_or_else(|error| panic!("{} reset failed: {error}", root.display()));
    }
    fs::create_dir_all(&root)
        .unwrap_or_else(|error| panic!("{} creation failed: {error}", root.display()));
    let database = root.join("database.sqlite3");
    let store = EventStore::open(&database, "ia0-failed-probe")
        .unwrap_or_else(|error| panic!("ready database creation failed: {error}"));
    drop(store);
    let migrations = ProductionMigrations::load()
        .unwrap_or_else(|error| panic!("production migration load failed: {error}"));
    match apply_migrations(&database, &root, "0.1.0", &migrations.as_slice()) {
        Ok(MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent) => {}
        Err(error) => panic!("ready database migration failed: {error}"),
    }
    let config = "slot=product\ninstance=instance-a\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms=1\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n";
    fs::write(root.join("service.conf"), config)
        .unwrap_or_else(|error| panic!("config write failed: {error}"));
    fs::write(root.join("basic.credential"), "alpha:zephyr")
        .unwrap_or_else(|error| panic!("credential write failed: {error}"));
    root
}

/// Our own probe listener, identified while this test still holds it.
///
/// On Linux its release is proven by the listening socket's inode leaving
/// this process's descriptor table, which no parallel test can influence.
/// Re-binding the released port instead can lose a race to any other test
/// that takes the same ephemeral port first (EADDRINUSE under full-suite
/// load, carrier run 20260926023421-4b55f408c5a74d55).
struct ListenerWitness {
    #[cfg(not(target_os = "linux"))]
    address: std::net::SocketAddr,
    #[cfg(target_os = "linux")]
    inode: u64,
}

impl ListenerWitness {
    fn of(listener: &grapt_location::BoundListener) -> Self {
        let address = listener.receipt().observed();
        Self {
            #[cfg(not(target_os = "linux"))]
            address,
            #[cfg(target_os = "linux")]
            inode: listening_inode(address),
        }
    }

    fn assert_released(&self, label: &str) {
        #[cfg(target_os = "linux")]
        assert_socket_closed(self.inode, label);
        #[cfg(not(target_os = "linux"))]
        assert_closed_probe_address_released(self.address, label);
    }
}

/// The inode of the one LISTEN socket bound to `address`. While we hold the
/// listener, no other socket can listen on that address.
#[cfg(target_os = "linux")]
fn listening_inode(address: std::net::SocketAddr) -> u64 {
    let std::net::SocketAddr::V4(v4) = address else {
        panic!("the probe listener is not IPv4 loopback: {address}");
    };
    let local = format!(
        "{:08X}:{:04X}",
        u32::from_ne_bytes(v4.ip().octets()),
        v4.port()
    );
    let table = fs::read_to_string("/proc/self/net/tcp")
        .unwrap_or_else(|error| panic!("socket table read failed: {error}"));
    let inodes: Vec<u64> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let listening = fields.get(1) == Some(&local.as_str()) && fields.get(3) == Some(&"0A");
            listening
                .then(|| fields.get(9).and_then(|inode| inode.parse().ok()))
                .flatten()
        })
        .collect();
    match inodes.as_slice() {
        [inode] => *inode,
        other => panic!("expected exactly our listener on {address}, found inodes {other:?}"),
    }
}

/// No descriptor of this process still refers to the socket `inode`.
#[cfg(target_os = "linux")]
fn assert_socket_closed(inode: u64, label: &str) {
    let target = format!("socket:[{inode}]");
    let descriptors = fs::read_dir("/proc/self/fd")
        .unwrap_or_else(|error| panic!("descriptor table read failed: {error}"));
    for entry in descriptors.flatten() {
        // A descriptor closed while we scan is simply gone: not ours.
        if fs::read_link(entry.path()).is_ok_and(|link| link.as_os_str() == target.as_str()) {
            panic!("HYG105 {label} leaked its listener: {target} is still open in this process");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn assert_closed_probe_address_released(address: std::net::SocketAddr, label: &str) {
    let socket = tokio::net::TcpSocket::new_v4()
        .unwrap_or_else(|error| panic!("{label} rebind socket creation failed: {error}"));
    socket
        .set_reuseaddr(true)
        .unwrap_or_else(|error| panic!("{label} rebind option failed: {error}"));
    socket
        .bind(address)
        .unwrap_or_else(|error| panic!("{label} leaked host address: {error}"));
    let rebound = socket
        .listen(1)
        .unwrap_or_else(|error| panic!("{label} leaked listener: {error}"));
    drop(rebound);
}

#[tokio::test]
async fn failed_authenticated_self_probe_creates_no_registration_artifacts() {
    let root = ready_service_root("failed-probe");
    let config = read_config(&root.join("service.conf"), "build-a")
        .unwrap_or_else(|error| panic!("config read failed: {error}"));
    let credential = read_credential(&root.join("basic.credential"))
        .unwrap_or_else(|error| panic!("credential read failed: {error}"));
    let refusal = run_ready(&root, config, credential, "wrong-secret".to_owned())
        .await
        .err()
        .unwrap_or_else(|| panic!("failed authenticated probe launched the product"));
    assert_eq!(
        refusal.to_string(),
        "authenticated self-probe refused: the probe returned status 401"
    );
    assert!(
        !root.join("registration").exists(),
        "failed probe created registration"
    );
    assert!(
        !root.join("registration.elected").exists(),
        "failed probe acquired controller authority"
    );
    assert!(
        !root.join("registration.lock").exists(),
        "failed probe acquired registration lock"
    );
}

#[tokio::test]
async fn registration_failure_stops_host_before_refusal() {
    let _socket_witness = SOCKET_WITNESS_CUSTODY.lock().await;
    let root = ready_service_root("registration-failure");
    // Startup now rejects corrupt snapshots before starting the host. This
    // witness must still reach the post-probe controller failure path.
    fs::create_dir(root.join("registration.elected"))
        .unwrap_or_else(|error| panic!("election obstruction creation failed: {error}"));
    let config = read_config(&root.join("service.conf"), "build-a")
        .unwrap_or_else(|error| panic!("config read failed: {error}"));
    let credential = read_credential(&root.join("basic.credential"))
        .unwrap_or_else(|error| panic!("credential read failed: {error}"));
    let listener = bind_loopback()
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let witness = ListenerWitness::of(&listener);
    let refusal = run_ready_on_listener(&root, config, credential, "zephyr".to_owned(), listener)
        .await
        .err()
        .unwrap_or_else(|| panic!("obstructed registration launched the product"));
    assert!(
        matches!(refusal, ProductRefusal::Registration(_)),
        "registration failure changed class: {refusal}"
    );
    witness.assert_released("registration refusal");
}

#[tokio::test]
async fn ready_custody_failure_drains_and_releases_registration() {
    let _socket_witness = SOCKET_WITNESS_CUSTODY.lock().await;
    let root = ready_service_root("ready-custody-failure");
    fs::create_dir(root.join(ready::READY_RECEIPT_FILE))
        .unwrap_or_else(|error| panic!("ready obstruction creation failed: {error}"));
    let config = read_config(&root.join("service.conf"), "build-a")
        .unwrap_or_else(|error| panic!("config read failed: {error}"));
    let credential = read_credential(&root.join("basic.credential"))
        .unwrap_or_else(|error| panic!("credential read failed: {error}"));
    let listener = bind_loopback()
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let witness = ListenerWitness::of(&listener);
    let refusal = run_ready_on_listener(&root, config, credential, "zephyr".to_owned(), listener)
        .await
        .err()
        .unwrap_or_else(|| panic!("unpersisted ready receipt launched the product"));
    assert!(
        matches!(refusal, ProductRefusal::Custody(_)),
        "ready custody failure changed class: {refusal}"
    );
    assert!(
        !root.join("registration").exists(),
        "ready custody failure left registration ownership behind"
    );
    witness.assert_released("ready custody refusal");
}

#[test]
fn non_release_outcome_is_never_reported_as_released() {
    assert!(
        !registration_was_released(&StopOutcome::SlotAlreadyEmpty),
        "non-release outcome was reported as released"
    );
}

#[tokio::test]
async fn drain_releases_the_observed_address_before_process_exit() {
    let _socket_witness = SOCKET_WITNESS_CUSTODY.lock().await;
    let listener = bind_loopback()
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let limits = host_limits().unwrap_or_else(|error| panic!("{error}"));
    let config = ServiceConfig {
        slot: "product".to_owned(),
        instance: "instance-a".to_owned(),
        environment: "internal-alpha".to_owned(),
        build: "build-a".to_owned(),
        version: "0.1.0".to_owned(),
        serve_window_ms: 1,
        provider_connect_ms: 10_000,
        provider_response_head_ms: 60_000,
        provider_active_attempts: 4,
        close_grace_ms: None,
        max_inflight_streams: None,
    };
    let credential = Credential {
        username: "alpha".to_owned(),
        secret: "zephyr".to_owned(),
    };
    let service = product_service(
        &limits,
        &credential,
        ProcessId::new(41).unwrap_or_else(|| panic!("pid was refused")),
        &config,
        None,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let witness = ListenerWitness::of(&listener);
    let host = PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
    .start();
    let _exit = drain_host(host).await;
    witness.assert_released("drained host");
}

fn read_json(path: &Path) -> Value {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("{} is invalid JSON: {error}", path.display()))
}

fn fixture_root_from(
    fixtures: Option<std::ffi::OsString>,
    artifact_root: Option<std::ffi::OsString>,
) -> Result<PathBuf, &'static str> {
    fixtures
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            artifact_root
                .filter(|value| !value.is_empty())
                .map(|root| PathBuf::from(root).join("fixtures/rust-reconstruction"))
        })
        .ok_or(
            "required frozen fixtures are unavailable; set SECODE_FIXTURES or run through the governed remote-compute artifact input",
        )
}

fn frozen_fixtures() -> PathBuf {
    fixture_root_from(
        std::env::var_os("SECODE_FIXTURES"),
        std::env::var_os("REMOTE_COMPUTE_ARTIFACT_ROOT"),
    )
    .unwrap_or_else(|refusal| panic!("{refusal}"))
}

#[test]
fn missing_fixture_configuration_is_refused() {
    assert_eq!(
        fixture_root_from(None, None),
        Err(
            "required frozen fixtures are unavailable; set SECODE_FIXTURES or run through the governed remote-compute artifact input"
        )
    );
}

#[test]
fn service_config_cannot_author_product_build_identity() {
    let root = ready_service_root("build-authority");
    let path = root.join("service.conf");
    let without_build = read_config(&path, "published-build")
        .unwrap_or_else(|error| panic!("published build was refused: {error}"));
    assert_eq!(without_build.build, "published-build");
    let with_build = "slot=product\ninstance=instance-a\nenvironment=internal-alpha\nbuild=shadow-build\nversion=0.1.0\nserve_window_ms=1\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n";
    fs::write(&path, with_build)
        .unwrap_or_else(|error| panic!("shadow config write failed: {error}"));
    let refusal = read_config(&path, "published-build")
        .err()
        .unwrap_or_else(|| panic!("service.conf shadowed PublishedConfig build"));
    assert!(refusal.to_string().contains("unknown config key \"build\""));
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    match pattern {
        "^[a-z0-9]+(?:-[a-z0-9]+)*$" => {
            !value.is_empty()
                && value.split('-').all(|part| {
                    !part.is_empty()
                        && part
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                })
        }
        "^[0-9a-f]{7,64}$" => {
            (7..=64).contains(&value.len())
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }
        "^[0-9a-f]{64}$" => {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }
        _ => false,
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn schema_matches(schema: &Value, value: &Value) -> bool {
    let Some(rules) = schema.as_object() else {
        return schema != &Value::Bool(false);
    };
    const KNOWN: [&str; 16] = [
        "$schema",
        "$id",
        "title",
        "description",
        "type",
        "const",
        "minLength",
        "pattern",
        "required",
        "additionalProperties",
        "properties",
        "minItems",
        "maxItems",
        "items",
        "not",
        "oneOf",
    ];
    if rules
        .keys()
        .any(|keyword| !KNOWN.contains(&keyword.as_str()))
    {
        return false;
    }
    let wrong_type =
        rules
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|expected| match expected {
                "object" => !value.is_object(),
                "array" => !value.is_array(),
                "string" => !value.is_string(),
                "integer" => value.as_i64().is_none() && value.as_u64().is_none(),
                _ => true,
            });
    if wrong_type || rules.get("const").is_some_and(|constant| constant != value) {
        return false;
    }
    if rules
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| {
            value
                .as_str()
                .is_none_or(|text| text.len() < minimum as usize)
        })
    {
        return false;
    }
    if rules
        .get("pattern")
        .and_then(Value::as_str)
        .is_some_and(|pattern| {
            value
                .as_str()
                .is_none_or(|text| !pattern_matches(pattern, text))
        })
    {
        return false;
    }
    if let Some(required) = rules.get("required").and_then(Value::as_array) {
        let Some(instance) = value.as_object() else {
            return false;
        };
        if required
            .iter()
            .filter_map(Value::as_str)
            .any(|name| !instance.contains_key(name))
        {
            return false;
        }
    }
    if let Some(properties) = rules.get("properties").and_then(Value::as_object) {
        let Some(instance) = value.as_object() else {
            return false;
        };
        if rules.get("additionalProperties") == Some(&Value::Bool(false))
            && instance.keys().any(|name| !properties.contains_key(name))
        {
            return false;
        }
        if properties.iter().any(|(name, property_schema)| {
            instance
                .get(name)
                .is_some_and(|property| !schema_matches(property_schema, property))
        }) {
            return false;
        }
    }
    if let Some(items) = value.as_array() {
        let length = items.len() as u64;
        if rules
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return false;
        }
        if rules
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return false;
        }
        if rules
            .get("items")
            .is_some_and(|item_schema| items.iter().any(|item| !schema_matches(item_schema, item)))
        {
            return false;
        }
    }
    if rules
        .get("not")
        .is_some_and(|forbidden| schema_matches(forbidden, value))
    {
        return false;
    }
    rules
        .get("oneOf")
        .and_then(Value::as_array)
        .is_none_or(|choices| {
            choices
                .iter()
                .filter(|choice| schema_matches(choice, value))
                .count()
                == 1
        })
}

#[test]
fn acceptance_corpus_is_schema_valid_closed_and_executable() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/ia0-product-root-v1");
    let manifest = read_json(&root.join("manifest.json"));
    let schema = read_json(&frozen_fixtures().join("oracle-receipt-manifest.schema.json"));
    assert!(
        schema_matches(&schema, &manifest),
        "manifest violates the ratified schema"
    );
    let actual_top_level: BTreeSet<&str> = manifest
        .as_object()
        .unwrap_or_else(|| panic!("manifest root is not an object"))
        .keys()
        .map(String::as_str)
        .collect();
    let expected_top_level: BTreeSet<&str> = [
        "$schema",
        "schema_version",
        "id",
        "title",
        "oracle",
        "scope",
        "determinism",
        "paths",
        "record_contracts",
        "evidence",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        actual_top_level, expected_top_level,
        "manifest top-level contract drifted"
    );
    let mut malformed = manifest.clone();
    malformed["unexpected"] = Value::Bool(true);
    assert!(
        !schema_matches(&schema, &malformed),
        "schema validator admitted an undeclared manifest field"
    );
    let mut malformed_pattern = manifest.clone();
    malformed_pattern["id"] = Value::String("INVALID".to_owned());
    assert!(
        !schema_matches(&schema, &malformed_pattern),
        "pattern validator admitted an invalid manifest id"
    );

    let actions = fs::read_to_string(root.join("actions.jsonl"))
        .unwrap_or_else(|error| panic!("actions are unreadable: {error}"));
    let durable = fs::read_to_string(root.join("expected/durable.jsonl"))
        .unwrap_or_else(|error| panic!("durable rows are unreadable: {error}"));
    let mut action_cases = BTreeSet::new();
    for line in actions.lines() {
        let row: Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("invalid action row: {error}"));
        let case = row["case"]
            .as_str()
            .unwrap_or_else(|| panic!("action omitted case"));
        let test = row["test"]
            .as_str()
            .unwrap_or_else(|| panic!("action omitted test"));
        let owner = row["owner"]
            .as_str()
            .unwrap_or_else(|| panic!("action omitted owner"));
        let source = match owner {
            "grapt-product/tests/product_root.rs" => include_str!("../../tests/product_root.rs"),
            "grapt-service/tests/authenticated_health_probe.rs" => {
                include_str!("../../../grapt-service/tests/authenticated_health_probe.rs")
            }
            other => panic!("unknown acceptance owner {other}"),
        };
        assert!(
            source.contains(&format!("fn {test}")),
            "owner {owner} has no test {test}"
        );
        assert!(
            action_cases.insert(case.to_owned()),
            "duplicate action case {case}"
        );
    }
    let durable_cases: BTreeSet<String> = durable
        .lines()
        .map(|line| {
            let row: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid durable row: {error}"));
            row["case"]
                .as_str()
                .unwrap_or_else(|| panic!("durable row omitted case"))
                .to_owned()
        })
        .collect();
    assert_eq!(action_cases, durable_cases, "corpus cases are not closed");
    let state = read_json(&root.join("expected/state.json"));
    let cases: Vec<_> = action_cases.iter().cloned().collect();
    assert_eq!(
        state,
        serde_json::json!({"case_count": cases.len(), "cases": cases})
    );
    let waivers = read_json(&root.join("expected/waivers.json"));
    assert_eq!(
        waivers["intentionally_excludes"],
        manifest["scope"]["intentionally_excludes"]
    );
}
