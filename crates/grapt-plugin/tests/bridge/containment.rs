//! SC-MAC-02: every platform's supervisor names how it contains a bridge,
//! and only the kernel kinds claim a boundary the tree cannot leave.

use super::*;
use grapt_plugin::ContainmentKind;

#[test]
fn the_supervisor_reports_its_platform_containment_honestly() -> Result<(), Box<dyn Error>> {
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let kind = supervisor.containment();
    let expected = if cfg!(target_os = "linux") {
        ContainmentKind::KernelCgroup
    } else if cfg!(windows) {
        ContainmentKind::KernelJob
    } else {
        ContainmentKind::Watchdog
    };
    assert_eq!(kind, expected);
    let description = kind.description();
    if kind == ContainmentKind::Watchdog {
        assert!(description.contains("not kernel-enforced"), "{description}");
        assert!(
            description
                .contains("leaves the bridge's process group and parent chain is not contained"),
            "{description}"
        );
    } else {
        assert!(description.starts_with("kernel-enforced"), "{description}");
    }
    Ok(())
}
