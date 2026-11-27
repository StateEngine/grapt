//! Real Windows signatures enter through the release manifest and staging path.
use super::*;
use serde_json::Value;

fn signed_fixture(root: &Path) -> Result<PathBuf> {
    let system = std::env::var_os("SystemRoot").ok_or("SystemRoot is absent")?;
    let signed = PathBuf::from(system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let publisher = super::super::windows_signing::read(&signed)?
        .signer
        .ok_or("system signature has no publisher")?;
    let bytes = std::fs::read(&signed)?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let source = fixture(root, A)?;
    let receipt_path = source.join("build-receipt.json");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&receipt_path)?)?;
    for name in NAMES {
        std::fs::write(source.join(name), &bytes)?;
        receipt["outputs"][name] = json!({"sha256":sha, "bytes":bytes.len()});
    }
    let raw = serde_json::to_vec(&receipt)?;
    std::fs::write(receipt_path, &raw)?;
    let receipt_sha = format!("{:x}", Sha256::digest(&raw));
    let manifest_path = source.join("release-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["publisher"] = json!(publisher);
    for name in NAMES {
        manifest["binaries"][name]["sha256"] = json!(sha);
        manifest["binaries"][name]["bytes"] = json!(bytes.len());
        manifest["binaries"][name]["build_receipt_sha256"] = json!(receipt_sha);
        manifest["signing"][name] = json!({"signed":true, "state":"signed", "status":"Valid",
            "verifier":"Get-AuthenticodeSignature", "signer":publisher,
            "verified_sha256":sha, "verified_bytes":bytes.len()});
    }
    let payload = source.join("stateengine");
    std::fs::create_dir(&payload)?;
    std::fs::write(payload.join("se.exe"), &bytes)?;
    std::fs::write(payload.join("NOTICE.txt"), b"fixture legal notice")?;
    let mut files = serde_json::Map::new();
    for name in ["se.exe", "NOTICE.txt"] {
        let body = std::fs::read(payload.join(name))?;
        files.insert(
            name.into(),
            json!({"sha256":format!("{:x}", Sha256::digest(&body)), "bytes":body.len()}),
        );
    }
    let raw = serde_json::to_vec(&json!({
        "schema":"stateengine.native-payload/v1", "name":"stateengine",
        "runtime":"rust-embedded", "version":"0.1.15", "source_commit":A,
        "target":"x86_64-pc-windows-msvc", "entry":"se.exe",
        "license_path":"NOTICE.txt", "notices_path":"NOTICE.txt", "files":files,
    }))?;
    std::fs::write(payload.join("native-payload.json"), &raw)?;
    let digest = format!("{:x}", Sha256::digest(&raw));
    files.insert(
        "native-payload.json".into(),
        json!({"sha256":digest, "bytes":raw.len()}),
    );
    let assets: serde_json::Map<String, Value> = files
        .into_iter()
        .map(|(name, row)| (format!("stateengine/{name}"), row))
        .collect();
    manifest["stateengine"] = json!({
        "status":"included", "runtime":"rust-embedded", "version":"0.1.15",
        "source_commit":A, "target":"x86_64-pc-windows-msvc",
        "entry":"stateengine/se.exe", "manifest_sha256":digest, "assets":assets,
    });
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    Ok(source)
}

#[test]
fn signed_packages_verify_through_literal_typographic_apostrophe_paths() -> Result {
    let root = Scratch::new()?;
    for quote in ['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
        let quoted = root.0.join(format!("signed {quote} package"));
        std::fs::create_dir(&quoted)?;
        let source = signed_fixture(&quoted)?;
        Package::verify(&source)?;
    }
    Ok(())
}

#[test]
fn signature_verification_never_substitutes_a_lossy_path_alias() -> Result {
    use std::os::windows::ffi::OsStringExt;
    let root = Scratch::new()?;
    let alias = root.0.join("signed \u{fffd}");
    let mut opaque = std::ffi::OsString::from("signed ");
    opaque.push(std::ffi::OsString::from_wide(&[0xd800]));
    let original = root.0.join(opaque);
    std::fs::create_dir(&alias)?;
    std::fs::create_dir(&original)?;
    let signed = signed_fixture(&alias)?;
    let claim: Value =
        serde_json::from_slice(&std::fs::read(signed.join("release-manifest.json"))?)?;
    let unsigned = fixture(&original, A)?;
    let manifest_path = unsigned.join("release-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["publisher"] = claim["publisher"].clone();
    for name in NAMES {
        manifest["signing"][name] = json!({"signed": true});
    }
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    assert!(
        Package::verify(&unsigned).is_err(),
        "the signed Unicode sibling must not attest the unsigned original"
    );
    Ok(())
}

#[test]
fn signed_packages_stage_and_activate_only_with_live_matching_signatures() -> Result {
    let root = Scratch::new()?;
    let quoted = root.0.join("signed ' package");
    std::fs::create_dir(&quoted)?;
    let source = signed_fixture(&quoted)?;
    let verified = Package::verify(&source)?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let installed = stage::install(&prefix, &verified)?;
    transition::activate_verified(&prefix, &installed)?;
    assert_eq!(windows_pointer::read(&prefix)?.as_deref(), Some(A));
    assert_eq!(
        installed.stateengine_entry.as_deref(),
        Some(Path::new("stateengine/se.exe"))
    );
    drop((installed, verified));
    let manifest_path = source.join("release-manifest.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    for fault in ["publisher", "unsigned", "missing-publisher", "forged-bytes"] {
        let mut manifest = original.clone();
        match fault {
            "publisher" => manifest["publisher"] = json!("CN=Unrelated Fixture Publisher"),
            "unsigned" => manifest["signing"][NAMES[0]]["signed"] = json!(false),
            "missing-publisher" => manifest["publisher"] = Value::Null,
            "forged-bytes" => {
                let bytes = b"not a signed executable";
                std::fs::write(source.join(NAMES[0]), bytes)?;
                manifest["binaries"][NAMES[0]]["sha256"] =
                    json!(format!("{:x}", Sha256::digest(bytes)));
                manifest["binaries"][NAMES[0]]["bytes"] = json!(bytes.len());
            }
            _ => return Err("unknown signing fault".into()),
        }
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
        let error = Package::verify(&source)
            .err()
            .ok_or("invalid signing claim was accepted")?;
        let reason = match fault {
            "publisher" => "signer differs",
            "unsigned" => "signing claim is malformed",
            "missing-publisher" => "signing claim cannot be verified",
            "forged-bytes" => "signature is not Valid",
            _ => return Err("unknown signing fault".into()),
        };
        assert!(error.contains(reason), "{fault}: {error}");
    }
    Ok(())
}
