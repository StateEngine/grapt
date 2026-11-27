//! Copyright 2025 OpenAI.
//! Apache-2.0. Safe library APIs; installed only in the dedicated inner helper.
use nix::libc;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;

pub(super) fn program(network: bool) -> Result<BpfProgram, String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => TargetArch::x86_64,
        "aarch64" => TargetArch::aarch64,
        _ => return Err("Linux sandbox seccomp is unavailable for this architecture".into()),
    };
    let mut rules = BTreeMap::<i64, Vec<SeccompRule>>::new();
    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_open_by_handle_at,
        libc::SYS_pidfd_getfd,
    ] {
        rules.insert(syscall, Vec::new());
    }
    if !network {
        for syscall in [
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_sendto,
            libc::SYS_sendmsg,
            libc::SYS_sendmmsg,
            libc::SYS_socket,
        ] {
            rules.insert(syscall, Vec::new());
        }
    } else {
        // Host Unix daemons can launch work outside this mount namespace.
        // Permit IP networking, but never reopen host Unix or VM sockets.
        let conditions = [libc::AF_INET, libc::AF_INET6]
            .into_iter()
            .map(|family| {
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, family as u64)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rules.insert(
            libc::SYS_socket,
            vec![SeccompRule::new(conditions).map_err(|e| e.to_string())?],
        );
    }
    // Local socketpairs remain usable by toolchains and their children; they
    // cannot connect to a pre-existing host endpoint.
    let condition = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Ne,
        libc::AF_UNIX as u64,
    )
    .map_err(|e| e.to_string())?;
    rules.insert(
        libc::SYS_socketpair,
        vec![SeccompRule::new(vec![condition]).map_err(|e| e.to_string())?],
    );
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| e.to_string())?;
    #[cfg(target_arch = "x86_64")]
    let program = {
        // x32 shares AUDIT_ARCH_X86_64 but changes syscall numbers. Refuse its
        // entire table rather than let an alternate ABI bypass the deny list.
        let mut guarded = vec![
            seccompiler::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            }, // LD W ABS nr
            seccompiler::sock_filter {
                code: 0x45,
                jt: 0,
                jf: 1,
                k: 0x4000_0000,
            }, // JSET x32
            seccompiler::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
            },
        ];
        guarded.extend(program);
        guarded
    };
    Ok(program)
}

pub(super) fn install(network: bool) -> Result<(), String> {
    let program = program(network)?;
    nix::sys::prctl::set_no_new_privs()
        .map_err(|e| format!("Linux sandbox no-new-privileges failed: {e}"))?;
    seccompiler::apply_filter(&program).map_err(|e| format!("Linux sandbox seccomp failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_ENV: &str = "GRAPT_SECCOMP_SENDMSG_PROBE";

    #[test]
    fn sendmsg_probe_child() -> Result<(), Box<dyn std::error::Error>> {
        let Some(mode) = std::env::var_os(PROBE_ENV) else {
            return Ok(());
        };
        let network = mode == "on";
        install(network)?;
        let status = std::process::Command::new("python3")
            .args([
                "-c",
                "import socket, sys\na, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)\ntry:\n    a.sendmsg([b'probe'])\n    sys.exit(0)\nexcept PermissionError:\n    sys.exit(77)\n",
            ])
            .status()?;
        let expected = if network { 0 } else { 77 };
        assert_eq!(status.code(), Some(expected));
        Ok(())
    }

    #[test]
    fn network_off_seccomp_blocks_sendmsg_with_eperm() -> Result<(), Box<dyn std::error::Error>> {
        let exe = std::env::current_exe()?;
        for mode in ["off", "on"] {
            let output = std::process::Command::new(&exe)
                .env(PROBE_ENV, mode)
                .args([
                    "--exact",
                    "linux::seccomp::tests::sendmsg_probe_child",
                    "--nocapture",
                ])
                .output()?;
            assert!(
                output.status.success(),
                "seccomp sendmsg probe ({mode}) failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
}
