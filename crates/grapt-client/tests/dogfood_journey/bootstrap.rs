//! Fresh-store preparation through the shipped product, without a store dependency.
use super::{fs, isolate_catalog, platform_config, Command, Path, SERVE_WINDOW_MS};

pub(super) fn bootstrap_store(binary: &Path, root: &Path) {
    let service_path = root.join("service.conf");
    let service = fs::read_to_string(&service_path)
        .unwrap_or_else(|error| panic!("bootstrap service config failed: {error}"));
    let short = service.replace(
        &format!("serve_window_ms={SERVE_WINDOW_MS}\n"),
        "serve_window_ms=100\n",
    );
    fs::write(&service_path, short)
        .unwrap_or_else(|error| panic!("bootstrap service window failed: {error}"));
    let output = isolate_catalog(&mut Command::new(binary), root, true)
        .args(["serve", "--service"])
        .arg(root)
        .args(["--port", "0"])
        .output();
    fs::write(&service_path, service)
        .unwrap_or_else(|error| panic!("journey service window restore failed: {error}"));
    let output = output.unwrap_or_else(|error| panic!("bootstrap spawn failed: {error}"));
    assert_initialized(output.status.success(), &output.stdout, root);
    // The bootstrap server has retired; only the next server may supply readiness.
    match fs::remove_file(root.join("serve-ready.json")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("bootstrap readiness removal failed: {error}"),
    }
    fs::write(root.join("grapt.jsonc"), platform_config("open-ready"))
        .unwrap_or_else(|error| panic!("open-ready config failed: {error}"));
}

fn assert_initialized(success: bool, stdout: &[u8], root: &Path) {
    let receipt: serde_json::Value = serde_json::from_slice(stdout)
        .unwrap_or_else(|error| panic!("fresh initialization returned no launch receipt: {error}"));
    assert!(
        success
            && receipt["schema"] == "grapt.product.launch.v1"
            && receipt["outcome"] == "served"
            && receipt["registration_released"] == true,
        "fresh initialization must serve and release its registration: {receipt}"
    );
    assert!(
        root.join("database.sqlite3").is_file(),
        "fresh initialization left no Ready database"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    const SERVED: &[u8] =
        br#"{"schema":"grapt.product.launch.v1","outcome":"served","registration_released":true}"#;

    #[test]
    fn bootstrap_guard_accepts_specific_served_receipt() {
        let root = super::super::unique_root("bootstrap-guard");
        fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
        fs::write(root.join("database.sqlite3"), b"fixture")
            .unwrap_or_else(|error| panic!("fixture database: {error}"));
        assert_initialized(true, SERVED, &root);
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("fixture cleanup: {error}"));
    }

    #[test]
    #[should_panic(expected = "must serve and release")]
    fn bootstrap_guard_refuses_config_failure() {
        assert_initialized(false, SERVED, Path::new("unused"));
    }

    #[test]
    #[should_panic(expected = "must serve and release")]
    fn bootstrap_guard_refuses_legacy_migration_refusal() {
        assert_initialized(true, br#"{"schema":"grapt.product.launch.v1","outcome":"refused","registration_released":true}"#, Path::new("unused"));
    }

    #[test]
    #[should_panic(expected = "must serve and release")]
    fn bootstrap_guard_refuses_unreleased_registration() {
        assert_initialized(true, br#"{"schema":"grapt.product.launch.v1","outcome":"served","registration_released":false}"#, Path::new("unused"));
    }

    #[test]
    #[should_panic(expected = "no launch receipt")]
    fn bootstrap_guard_refuses_generic_success() {
        assert_initialized(true, b"", Path::new("unused"));
    }

    #[test]
    #[should_panic(expected = "no Ready database")]
    fn bootstrap_guard_refuses_missing_database() {
        let root = super::super::unique_root("bootstrap-missing");
        assert!(!root.exists(), "missing-database fixture must be absent");
        assert_initialized(true, SERVED, &root);
    }
}
