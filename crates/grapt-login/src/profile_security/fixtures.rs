//! Test-only credentials in the formats real accounts hold: a DPAPI envelope
//! sealed by Windows itself, and a keystore envelope naming a key the
//! keystore never held.
use super::{entropy, envelope};
use std::io;

#[cfg(windows)]
const PROTECT: &str = "Add-Type -AssemblyName System.Security; \
    $data = [Convert]::FromBase64String([Console]::In.ReadLine()); \
    $sealed = [Security.Cryptography.ProtectedData]::Protect($data, $null, \
    [Security.Cryptography.DataProtectionScope]::CurrentUser); \
    [Console]::Out.Write([Convert]::ToBase64String($sealed))";

/// `plaintext` sealed with CurrentUser DPAPI, in the earlier envelope.
#[cfg(windows)]
pub(crate) fn dpapi_sealed(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("SystemRoot is unset"))?;
    let mut child = Command::new(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            PROTECT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("stdin"))?;
    writeln!(input, "{}", STANDARD.encode(plaintext))?;
    drop(input);
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout"))?
        .read_to_string(&mut output)?;
    if !child.wait()?.success() {
        return Err(io::Error::other("DPAPI protection failed"));
    }
    let blob = STANDARD
        .decode(output.trim())
        .map_err(|_| io::Error::other("DPAPI output is not base64"))?;
    Ok([super::DPAPI_ENVELOPE, &blob].concat())
}

/// `plaintext` sealed in the keystore envelope under a fresh key identity
/// that was never stored, as a credential reads after its key is deleted.
pub(crate) fn sealed_with_missing_key(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    let mut id = [0; envelope::ID_BYTES];
    entropy::fill(&mut id)?;
    let mut nonce = [0; envelope::NONCE_BYTES];
    entropy::fill(&mut nonce)?;
    envelope::seal(&id, &[0; envelope::KEY_BYTES], nonce, plaintext)
}
