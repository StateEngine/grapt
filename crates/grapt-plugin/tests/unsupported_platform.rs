#![cfg(not(any(target_os = "linux", windows, target_os = "macos")))]

use crate::support;

use grapt_plugin::{
    BridgeRuntime, PluginError, PluginSupervisor, RuntimePin, DEFAULT_BRIDGE_LIMITS,
};

#[test]
fn unsupported_platform_refuses_before_runtime_io() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        std::env::temp_dir().join(format!("secode-plugin-unsupported-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let runtime = BridgeRuntime {
        executable: root.join("missing-node"),
        bridge: root.join("missing-bridge"),
        private_root: root.join("private"),
        recovery: support::recovery(&root.join("private")),
        expected: RuntimePin {
            version: "v24.0.0".to_owned(),
            executable_sha256: "a".repeat(64),
            bridge_sha256: "b".repeat(64),
        },
    };
    let outcome = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS);
    assert!(
        matches!(outcome, Err(PluginError::Runtime(ref reason))
            if reason == "plugin supervision is unsupported on this platform"),
        "unsupported platform did not refuse at containment admission: {outcome:?}"
    );
    assert!(std::fs::read_dir(&root)?.next().is_none());
    Ok(())
}
