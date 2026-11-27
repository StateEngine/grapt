//! Resource corruption, copy and low-descriptor regression witnesses.
use super::*;

#[test]
fn launcher_publication_retains_identity_and_rejects_content_or_mode_changes() -> Result {
    let scratch = Scratch::new()?;
    let temporary = scratch.0.join("launcher.new");
    let published = scratch.0.join("launcher");
    std::fs::write(&temporary, b"verified launcher")?;
    let (_, _, _, stamp) = super::super::package::fingerprint(&temporary, 1024)?;
    std::fs::rename(&temporary, &published)?;
    let stamp = stamp.after_rename(&published)?;
    stamp.recheck(&published)?;
    std::fs::rename(&published, &temporary)?;
    std::fs::write(&published, b"verified launcher")?;
    assert!(
        stamp.recheck(&published).is_err(),
        "same bytes are not the retained inode"
    );
    for change_mode in [false, true] {
        let (_, _, _, stamp) = super::super::package::fingerprint(&published, 1024)?;
        if change_mode {
            std::fs::set_permissions(&published, std::fs::Permissions::from_mode(0o700))?;
        } else {
            std::fs::write(&published, b"different content length")?;
        }
        assert!(stamp.after_rename(&published).is_err());
    }
    Ok(())
}

fn bundled(root: &Path, count: usize) -> Result {
    bundled_payload(root, count, false)
}

fn bundled_payload(root: &Path, count: usize, native: bool) -> Result {
    let runtime = root.join("stateengine");
    std::fs::create_dir(&runtime)?;
    let mut files = serde_json::Map::new();
    let names = if native {
        vec!["se", "NOTICE.txt"]
    } else {
        vec![
            "se",
            "node",
            "LICENSE",
            "THIRD_PARTY_NOTICES.md",
            "Node-LICENSE.txt",
        ]
    };
    for name in names {
        let body = format!("fixture {name}");
        std::fs::write(runtime.join(name), &body)?;
        std::fs::set_permissions(
            runtime.join(name),
            std::fs::Permissions::from_mode(if name == "se" || name == "node" {
                0o700
            } else {
                0o600
            }),
        )?;
        files.insert(
            name.into(),
            json!({"bytes":body.len(), "sha256":digest(body.as_bytes())}),
        );
    }
    for index in 0..count {
        let name = format!("resource-{index}");
        std::fs::write(runtime.join(&name), b"resource")?;
        files.insert(name, json!({"bytes":8,"sha256":digest(b"resource")}));
    }
    let mut document = json!({
        "schema": if native { "stateengine.native-payload/v1" } else { "stateengine.se-payload.v1" },
        "name":"stateengine", "version":"0.1.15",
        "source_commit":A, "target":"x86_64-unknown-linux-gnu", "entry":"se",
        "node":{"version":"24.15.0", "license_path":"Node-LICENSE.txt"},
        "license_path":"LICENSE", "notices_path":"THIRD_PARTY_NOTICES.md", "files":files,
    });
    if native {
        document
            .as_object_mut()
            .ok_or("fixture manifest is not an object")?
            .remove("node");
        document["runtime"] = json!("rust-embedded");
        document["license_path"] = json!("NOTICE.txt");
        document["notices_path"] = json!("NOTICE.txt");
    }
    let payload = serde_json::to_vec(&document)?;
    let manifest = if native {
        "native-payload.json"
    } else {
        "stateengine-payload.json"
    };
    std::fs::write(runtime.join(manifest), &payload)?;
    let mut assets = serde_json::Map::new();
    for (name, row) in files {
        assets.insert(format!("stateengine/{name}"), row);
    }
    assets.insert(
        format!("stateengine/{manifest}"),
        json!({"bytes":payload.len(), "sha256":digest(&payload)}),
    );
    change_manifest(root, |manifest| {
        manifest["stateengine"] = json!({
            "status":"included", "version":"0.1.15", "source_commit":A,
            "target":"x86_64-unknown-linux-gnu", "entry":"stateengine/se",
            "manifest_sha256":digest(&payload), "assets":assets,
        })
    })
}

#[test]
fn resources_install_and_revert_with_the_selected_package() -> Result {
    for native in [false, true] {
        let scratch = Scratch::new()?;
        let source = fixture(&scratch.0, A)?;
        bundled_payload(&source, 8, native)?;
        let package = Package::verify(&source)?;
        assert_eq!(
            package.stateengine_entry.as_deref(),
            Some(Path::new("stateengine/se"))
        );
        let prefix = scratch.0.join("installation");
        std::fs::create_dir(&prefix)?;
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
        let a = stage::install(&prefix, &package)?;
        transition::activate_verified(&prefix, &a)?;
        assert_eq!(
            Package::verify(&prefix.join("versions").join(A).join("bin"))?.stateengine_entry,
            package.stateengine_entry
        );
        let source_b = fixture(&scratch.0, B)?;
        bundled_payload(&source_b, 2, native)?;
        let b = stage::install(&prefix, &Package::verify(&source_b)?)?;
        transition::activate_verified(&prefix, &b)?;
        assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(B));
        let a = Package::verify(&prefix.join("versions").join(A).join("bin"))?;
        transition::activate_verified(&prefix, &a)?;
        assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(A));
        assert!(super::super::package::bundled_entry(&a.root)?.ends_with("stateengine/se"));
    }
    Ok(())
}

#[test]
fn resource_corruption_missing_extra_link_and_manifest_disagreement_refuse() -> Result {
    for native in [false, true] {
        for fault in [
            "corrupt", "missing", "extra", "link", "entry", "inner", "mode",
        ] {
            let scratch = Scratch::new()?;
            let source = fixture(&scratch.0, A)?;
            bundled_payload(&source, 1, native)?;
            let manifest = source.join(if native {
                "stateengine/native-payload.json"
            } else {
                "stateengine/stateengine-payload.json"
            });
            let resource = source.join("stateengine/resource-0");
            match fault {
                "corrupt" => std::fs::write(&resource, b"mutated!")?,
                "missing" => std::fs::remove_file(&resource)?,
                "extra" => std::fs::write(source.join("stateengine/extra"), b"unlisted")?,
                "link" => {
                    std::fs::remove_file(&resource)?;
                    std::os::unix::fs::symlink(source.join("QUICKSTART.md"), &resource)?;
                }
                "entry" => change_manifest(&source, |manifest| {
                    manifest["stateengine"]["entry"] = json!("../se")
                })?,
                "inner" => std::fs::write(&manifest, b"{}")?,
                "mode" => std::fs::set_permissions(
                    source.join("stateengine/se"),
                    std::fs::Permissions::from_mode(0o600),
                )?,
                _ => return Err("unknown resource fault".into()),
            }
            assert!(Package::verify(&source).is_err(), "{fault} must refuse");
        }
    }
    Ok(())
}

#[test]
fn post_verification_resource_mutation_cannot_activate_a_copy() -> Result {
    for extra in [false, true] {
        let scratch = Scratch::new()?;
        let source = fixture(&scratch.0, A)?;
        bundled(&source, 1)?;
        let package = Package::verify(&source)?;
        if extra {
            std::fs::write(source.join("stateengine/extra"), b"extra")?;
        } else {
            std::fs::write(source.join("stateengine/resource-0"), b"mutated!")?;
        }
        assert!(package.recheck(&source).is_err());
        let prefix = scratch.0.join("installation");
        std::fs::create_dir(&prefix)?;
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
        assert!(stage::install(&prefix, &package).is_err());
        assert!(transition::pointer(&prefix)?.is_none());
    }
    Ok(())
}

#[test]
fn resource_verification_does_not_retain_one_descriptor_per_file() -> Result {
    const MARKER: &str = "GRAPT_TEST_LOW_DESCRIPTOR_CHILD";
    if std::env::var_os(MARKER).is_some() {
        let scratch = Scratch::new()?;
        let source = fixture(&scratch.0, A)?;
        bundled(&source, 400)?;
        let package = Package::verify(&source)?;
        package.recheck(&source)?;
        let prefix = scratch.0.join("installation");
        std::fs::create_dir(&prefix)?;
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
        let installed = stage::install(&prefix, &package)?;
        assert!(installed.root.join("stateengine/resource-399").is_file());
        return Ok(());
    }
    let binary = std::env::current_exe()?;
    let output = std::process::Command::new("/usr/bin/timeout")
        .args(["30", "/bin/sh", "-c", "ulimit -n 128; exec \"$@\"", "low-fd-witness"])
        .arg(binary).args(["--exact", "installation::tests::stateengine_tests::resource_verification_does_not_retain_one_descriptor_per_file", "--nocapture", "--test-threads=1"])
        .env(MARKER, "1").output()?;
    assert!(
        output.status.success(),
        "low-descriptor witness failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "low-descriptor witness did not execute"
    );
    Ok(())
}

#[test]
fn fixed_se_launcher_resolves_current_without_relying_on_the_selected_cli() -> Result {
    let scratch = Scratch::new()?;
    let source = fixture(&scratch.0, A)?;
    bundled(&source, 1)?;
    let package = Package::verify(&source)?;
    let prefix = scratch.0.join("installation");
    std::fs::create_dir(&prefix)?;
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
    let a = stage::install(&prefix, &package)?;
    super::super::launchers::prepare_bundled(&prefix, &a)?;
    transition::activate_verified(&prefix, &a)?;
    let launcher = prefix.join("bin/se");
    let selected = super::super::bundled::selected_root(&launcher)?;
    assert_eq!(selected, a.root);
    assert!(super::super::package::bundled_entry(&selected).is_ok());
    // An older build has no StateEngine metadata or hidden CLI command.
    let b = stage::install(&prefix, &Package::verify(&fixture(&scratch.0, B)?)?)?;
    transition::activate_verified(&prefix, &b)?;
    let selected = super::super::bundled::selected_root(&launcher)?;
    assert_eq!(selected, b.root);
    assert!(super::super::package::bundled_entry(&selected)
        .is_err_and(|error| error.contains("does not include StateEngine")));
    transition::activate_verified(&prefix, &a)?;
    assert_eq!(super::super::bundled::selected_root(&launcher)?, a.root);
    // A foreign replacement at the stable name is refused, not overwritten.
    let replacement = vec![b'x'; std::fs::read(&launcher)?.len()];
    std::fs::write(&launcher, &replacement)?;
    assert!(
        super::super::launchers::prepare_bundled(&prefix, &a).is_err(),
        "unknown fixed launcher replacement was accepted"
    );
    assert_eq!(std::fs::read(&launcher)?, replacement);
    Ok(())
}

#[test]
fn an_interrupted_linux_fixed_launcher_refresh_recovers_only_known_requested_bytes() -> Result {
    let scratch = Scratch::new()?;
    let source_a = fixture(&scratch.0, A)?;
    bundled(&source_a, 1)?;
    let source_b = fixture(&scratch.0, B)?;
    bundled(&source_b, 1)?;
    let body = b"different verified synthetic CLI; never execute";
    std::fs::write(source_b.join("grapt"), body)?;
    let receipt_path = source_b.join("build.json");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&receipt_path)?)?;
    receipt["outputs"]["grapt"] = json!({"bytes":body.len(),"sha256":digest(body)});
    let receipt = serde_json::to_vec(&receipt)?;
    std::fs::write(receipt_path, &receipt)?;
    change_manifest(&source_b, |manifest| {
        manifest["binaries"]["grapt"]["bytes"] = json!(body.len());
        manifest["binaries"]["grapt"]["sha256"] = json!(digest(body));
        if let Some(binaries) = manifest["binaries"].as_object_mut() {
            for entry in binaries.values_mut() {
                entry["build_receipt_sha256"] = json!(digest(&receipt));
            }
        } else {
            panic!("fixture binary declarations are absent");
        }
    })?;
    let prefix = scratch.0.join("installation");
    std::fs::create_dir(&prefix)?;
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
    let a = stage::install(&prefix, &Package::verify(&source_a)?)?;
    super::super::launchers::prepare_bundled(&prefix, &a)?;
    transition::activate_verified(&prefix, &a)?;
    let b = stage::install(&prefix, &Package::verify(&source_b)?)?;
    assert!(
        super::super::launchers::interrupt_bundled_refresh(&prefix, &b)
            .is_err_and(|error| error.contains("named launcher publication interruption"))
    );
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(A));
    assert_eq!(std::fs::read(prefix.join("bin/se"))?, body);
    let interrupted: Value =
        serde_json::from_slice(&std::fs::read(prefix.join("se-bootstrap.json"))?)?;
    assert_eq!(interrupted["source_commit"], A);
    super::super::launchers::prepare_bundled(&prefix, &b)?;
    let recovered: Value =
        serde_json::from_slice(&std::fs::read(prefix.join("se-bootstrap.json"))?)?;
    assert_eq!(recovered["source_commit"], B);
    transition::activate_verified(&prefix, &b)?;
    assert_eq!(
        super::super::bundled::selected_root(&prefix.join("bin/se"))?,
        b.root
    );
    Ok(())
}
