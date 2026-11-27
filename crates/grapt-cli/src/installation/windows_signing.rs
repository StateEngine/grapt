//! Live Authenticode verification bound to the exact package bytes.

use serde::Deserialize;
use std::path::Path;

use super::{io, package, windows_reader};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Signature {
    status: String,
    pub(super) signer: Option<String>,
}

pub(super) fn read(path: &Path) -> Result<Signature, String> {
    let script = "$ErrorActionPreference = 'Stop'; \
         [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); \
         $s = Microsoft.PowerShell.Security\\Get-AuthenticodeSignature -LiteralPath $env:GRAPT_SECURITY_PATH; \
         [pscustomobject]@{status = $s.Status.ToString(); \
         signer = if ($s.SignerCertificate) { $s.SignerCertificate.Subject } else { $null } \
         } | ConvertTo-Json -Compress";
    let raw = windows_reader::powershell(script, path)?;
    if raw.len() > 16 * 1024 {
        return Err("package signature verifier output exceeds its bound".into());
    }
    let signature: Signature = serde_json::from_slice(&raw)
        .map_err(|_| "package signature verifier returned malformed output")?;
    if signature.status != "Valid" || signature.signer.as_deref().is_none_or(str::is_empty) {
        return Err(format!("package signature is not Valid ({}); obtain a valid signed package or an explicitly unsigned local package", signature.status));
    }
    Ok(signature)
}

pub(super) fn verify(path: &Path, publisher: &str, sha: &str, bytes: u64) -> Result<(), String> {
    // Deny writes and deletion while both hashing and Authenticode use this name.
    // A pre-existing writer also prevents acquiring this reader.
    let _immutable = io::immutable_regular(path)?;
    let (actual, length, _, stamp) = package::fingerprint(path, bytes)?;
    if !actual.eq_ignore_ascii_case(sha) || length != bytes {
        return Err("signed package binary differs from its attested bytes".into());
    }
    if read(path)?.signer.as_deref() != Some(publisher) {
        return Err("package signature signer differs from its declared publisher; obtain a package from the declared publisher".into());
    }
    stamp.recheck(path)
}
