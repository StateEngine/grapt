//! The acquisition child must finish even after its frontend owner has exited.
#[cfg(target_os = "linux")]
#[test]
fn stalled_clipboard_acquisition_does_not_outlive_its_parent_indefinitely(
) -> Result<(), Box<dyn std::error::Error>> {
    for flag in ["--clipboard-read-text", "--clipboard-read-image"] {
        let result = std::process::Command::new("python3")
        .args(["-c", r#"
import ctypes, os, socket, subprocess, sys, tempfile, time
assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0  # adopt only our orphaned child
listener = socket.socket()
listener.bind(('127.0.0.1', 0))
listener.listen(1)
listener.settimeout(5)
pid = None
connection = None
try:
    with tempfile.NamedTemporaryFile() as authentication:
        environment = dict(os.environ, DISPLAY='127.0.0.1:'+str(listener.getsockname()[1]-6000),
                           XAUTHORITY=authentication.name)
        owner = subprocess.run([sys.executable, '-c',
            'import os,subprocess,sys;p=subprocess.Popen(sys.argv[1:],stdin=subprocess.DEVNULL,'
            'stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print(p.pid,flush=True);os._exit(0)',
            sys.argv[1], sys.argv[2]], env=environment,
            capture_output=True, timeout=5, check=True)
        pid = int(owner.stdout)
        connection, _ = listener.accept()
        connection.settimeout(5)
        assert connection.recv(1), 'helper never reached X11 setup'
        # The fake X11 peer never replies. The real owner has already exited.
        for _ in range(1000):
            found, status = os.waitpid(pid, os.WNOHANG)
            if found:
                pid = None
                assert os.waitstatus_to_exitcode(status) == 1
                break
            time.sleep(.01)
        else:
            raise AssertionError('clipboard helper survived its own deadline')
finally:
    if pid is not None:
        try: os.kill(pid, 9)
        except ProcessLookupError: pass
        os.waitpid(pid, 0)
    if connection is not None: connection.close()
    listener.close()
"#, env!("CARGO_BIN_EXE_grapt-tui"), flag])
        .output()?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}
