//! Shipped CLI convergence over the ordinary provider transport.
use super::*;

/// Durable announcements let the shipped client converge on the completed run.
#[test]
fn a_provider_run_converges_through_the_shipped_cli() {
    let cli = shipped_cli::cli_binary();
    let root = root("provider-cli");
    reset(&root);
    let database = root.join("database.sqlite3");
    drop(
        EventStore::open(&database, "provider-cli-test")
            .unwrap_or_else(|error| panic!("store initialize failed: {error}")),
    );
    let migrations =
        ProductionMigrations::load().unwrap_or_else(|error| panic!("migrations absent: {error}"));
    apply_migrations(&database, &root, "0.1.0", &migrations.as_slice())
        .unwrap_or_else(|error| panic!("migration failed: {error}"));
    write_inputs(&root);
    let config = fs::read_to_string(root.join("service.conf"))
        .unwrap_or_else(|error| panic!("config read failed: {error}"));
    fs::write(
        root.join("service.conf"),
        config.replace("serve_window_ms=1000\n", "serve_window_ms=30000\n"),
    )
    .unwrap_or_else(|error| panic!("serve window write failed: {error}"));
    let provider = scripted_provider::Server::start(Vec::new(), |_| false)
        .unwrap_or_else(|error| panic!("provider fixture: {error}"));
    provider_profile::configure(&root, provider.address)
        .unwrap_or_else(|error| panic!("provider profile: {error}"));
    let mut product = RestartProcess::launch(&root);
    product.ready(&root);
    let run = Command::new(&cli)
        .current_dir(&root)
        .arg("run")
        .arg("--service")
        .arg(&root)
        .args(["--new", "--prompt", "provider continuity", "--jsonl"])
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("CLI launch failed: {error}"));
    product.kill_and_reap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "a provider run did not converge through the CLI: {stderr}"
    );
    let receipt = String::from_utf8_lossy(&run.stdout)
        .lines()
        .last()
        .map(str::to_owned)
        .unwrap_or_default();
    assert!(
        receipt.contains("\"schema\":\"grapt.cli.run-receipt.v1\""),
        "the CLI run ended without its receipt: {receipt}"
    );
}
