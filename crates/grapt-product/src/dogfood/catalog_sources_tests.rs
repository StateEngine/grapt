//! One source set for every reader (backlog item 8): the profile's own
//! `grapt.jsonc` heads it, as the user's; a repository never becomes the
//! user's by containing a service root.
use super::*;
use grapt_plugin::PluginOrigin;

fn scratch(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "grapt-sources-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn write(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, body).map_err(|error| error.to_string())
}

#[test]
fn seat01_explicit_launcher_config_keeps_the_user_owned_profile_source() -> Result<(), String> {
    let root = scratch("seat01-launcher");
    let profile = root.join("service/grapt.jsonc");
    let launcher = root.join("launcher.jsonc");
    write(
        &profile,
        r#"{"permission":{"stateengine_*":{"stateengine/*":"allow","stateengine/*/*":"ask"}}}"#,
    )?;
    write(
        &launcher,
        r#"{"mcp":{"stateengine":{"type":"local","command":["stateengine","mcp"]}}}"#,
    )?;
    std::fs::create_dir_all(root.join("work")).map_err(|error| error.to_string())?;
    let mut environment = CatalogEnvironment::isolated(&root);
    environment.explicit = Some(launcher.clone());
    let sources = CatalogSources::read(&root.join("work"), environment);
    let _ = std::fs::remove_dir_all(&root);
    let sources = sources?;
    let profile_index = sources
        .documents
        .iter()
        .position(|document| Path::new(&document.source) == profile)
        .ok_or("profile omitted")?;
    let launcher_index = sources
        .documents
        .iter()
        .position(|document| Path::new(&document.source) == launcher)
        .ok_or("explicit launcher omitted")?;
    assert!(profile_index < launcher_index);
    assert_eq!(
        sources.documents[profile_index].origin,
        PluginOrigin::UserGlobal
    );
    assert!(sources.documents[profile_index]
        .value
        .get("permission")
        .is_some());
    assert_eq!(
        sources.documents[launcher_index].origin,
        PluginOrigin::LaunchExplicit
    );
    assert!(sources.documents[launcher_index].value.get("mcp").is_some());
    Ok(())
}

/// A packaged install: the project is not under the service root. Every
/// reader still sees the profile's document, first and as the user's.
#[test]
fn the_profile_document_heads_the_sources_of_a_project_outside_it() -> Result<(), String> {
    let root = scratch("outside");
    let project = root.join("repos").join("work");
    write(
        &root.join("service").join("grapt.jsonc"),
        r#"{"compaction":{"auto":false}}"#,
    )?;
    write(&project.join("grapt.json"), r#"{"model":"x/y"}"#)?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root));
    let _ = std::fs::remove_dir_all(&root);
    let sources = sources?;
    let first = sources.documents.first().ok_or("no documents")?;
    assert!(first.source.ends_with("grapt.jsonc"), "{}", first.source);
    assert!(first.directory.ends_with("service"));
    assert_eq!(first.origin, PluginOrigin::UserGlobal);
    assert!(user_owned(first.origin));
    let last = sources.documents.last().ok_or("no project document")?;
    assert_eq!(last.origin, PluginOrigin::Project);
    assert!(!user_owned(last.origin));
    // Compaction reads the same set: the profile's setting applies here.
    let settings = super::super::compaction_config::CompactionSettings::fold(
        sources.documents.iter().map(|document| &document.value),
    )?;
    assert!(!settings.auto);
    Ok(())
}

/// A service root inside a project (test and bench roots under a repo): the
/// service root's own document is the user's, the project's stays the
/// project's, read once each.
#[test]
fn a_service_root_inside_a_project_does_not_promote_the_project() -> Result<(), String> {
    let root = scratch("inside");
    let service = root.join("service");
    write(&service.join("grapt.jsonc"), r#"{}"#)?;
    write(&root.join("grapt.json"), r#"{}"#)?;
    let sources = CatalogSources::read(&service, CatalogEnvironment::isolated(&root));
    let _ = std::fs::remove_dir_all(&root);
    let sources = sources?;
    let origin_of = |path: &Path| {
        sources
            .documents
            .iter()
            .filter(|document| Path::new(&document.source) == path)
            .map(|document| document.origin)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        origin_of(&service.join("grapt.jsonc")),
        vec![PluginOrigin::UserGlobal]
    );
    assert_eq!(
        origin_of(&root.join("grapt.json")),
        vec![PluginOrigin::Project]
    );
    Ok(())
}

/// The plugins of that layout: the service root's own plugin resolves; the
/// project's is blocked, by the executable gate, and is listed as blocked.
#[test]
fn a_project_plugin_beside_a_service_root_stays_blocked() -> Result<(), String> {
    let root = scratch("plugins");
    let service = root.join("service");
    write(&service.join("plugin.js"), "export default {}\n")?;
    write(
        &service.join("grapt.jsonc"),
        r#"{"plugins":["./plugin.js"]}"#,
    )?;
    write(&root.join("plugin.js"), "export default {}\n")?;
    write(&root.join("grapt.json"), r#"{"plugins":["./plugin.js"]}"#)?;
    let resolved = CatalogSources::read(&service, CatalogEnvironment::isolated(&root))
        .and_then(|sources| super::super::plugins::resolve_sources(&sources));
    let _ = std::fs::remove_dir_all(&root);
    let resolved = resolved?;
    assert_eq!(resolved.active.len(), 1, "{resolved:?}");
    assert_eq!(resolved.active[0].origin, PluginOrigin::UserGlobal);
    assert_eq!(resolved.blocked.len(), 1, "{resolved:?}");
    assert_eq!(resolved.blocked[0].origin, PluginOrigin::Project);
    assert!(
        resolved.blocked[0]
            .reason
            .contains("user-global or launch-explicit"),
        "{}",
        resolved.blocked[0].reason
    );
    Ok(())
}

/// A Session's plugin list: the project's blocked declaration is added to
/// the service's own list once; the profile's plugin is never among them.
#[test]
fn a_project_plugin_is_listed_as_blocked_once() -> Result<(), String> {
    let root = scratch("listed");
    let project = root.join("repos").join("work");
    write(
        &root.join("service").join("plugin.js"),
        "export default {}\n",
    )?;
    write(
        &root.join("service").join("grapt.jsonc"),
        r#"{"plugins":["./plugin.js"]}"#,
    )?;
    write(&project.join("plugin.js"), "export default {}\n")?;
    write(
        &project.join("grapt.json"),
        r#"{"plugins":["./plugin.js"]}"#,
    )?;
    let resolved = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))
        .and_then(|sources| super::super::plugins::resolve_sources(&sources));
    let _ = std::fs::remove_dir_all(&root);
    let resolved = resolved?;
    assert_eq!(resolved.active.len(), 1, "{resolved:?}");
    let listed = super::super::plugins::project_blocked(&resolved, &[])?;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(
        listed[0]["source"]
            .as_str()
            .is_some_and(|source| Path::new(source).starts_with(&project)),
        "{listed:?}"
    );
    assert!(super::super::plugins::project_blocked(&resolved, &listed)?.is_empty());
    Ok(())
}

/// A document no reader can use is refused by its path, never its contents.
#[test]
fn an_unreadable_document_is_refused_by_its_path() -> Result<(), String> {
    let root = scratch("unreadable");
    let project = root.join("repos").join("work");
    // Not a regular file: the project's document.
    let irregular = project.join("grapt.json");
    std::fs::create_dir_all(&irregular).map_err(|error| error.to_string())?;
    let not_file = CatalogSources::read(&project, CatalogEnvironment::isolated(&root)).err();
    std::fs::remove_dir(&irregular).map_err(|error| error.to_string())?;
    // Not a configuration object: the profile's document.
    let malformed = root.join("service").join("grapt.jsonc");
    write(&malformed, r#"["secret-value"]"#)?;
    let not_object = CatalogSources::read(&project, CatalogEnvironment::isolated(&root)).err();
    let _ = std::fs::remove_dir_all(&root);
    for (refusal, path) in [(not_file, &irregular), (not_object, &malformed)] {
        let refusal = refusal.ok_or("an unreadable source was accepted")?;
        let (_, named) = refusal
            .rsplit_once(": ")
            .ok_or("refusal omitted its path")?;
        assert_eq!(Path::new(named), path.as_path(), "{refusal}");
        assert!(!refusal.contains("secret-value"), "{refusal}");
    }
    Ok(())
}

#[test]
fn refusal_paths_use_consistent_separators() {
    let path = Path::new("project").join(".grapt").join("grapt.json");
    assert_eq!(
        refusal_text("not a regular file", &path),
        "not a regular file: project/.grapt/grapt.json"
    );
}

#[cfg(windows)]
#[test]
fn refusal_paths_preserve_unc_roots_with_consistent_separators() {
    let path = Path::new(r"\\server\share\project\grapt.json");
    assert_eq!(
        refusal_text("not a regular file", path),
        "not a regular file: //server/share/project/grapt.json"
    );
}

#[cfg(unix)]
#[test]
fn refusal_paths_preserve_a_backslash_inside_a_unix_directory_name() {
    let path = Path::new(r"/project/keep\literal/grapt.json");
    assert_eq!(
        refusal_text("not a regular file", path),
        r"not a regular file: /project/keep\literal/grapt.json"
    );
}

/// The review's long-path case: past a 256-byte notice reason,
/// two failed documents under a long directory must still be told apart by
/// name and reason, with their contents absent.
#[test]
fn long_path_refusals_keep_their_document_and_reason_in_a_notice() -> Result<(), String> {
    let root = scratch("long-path");
    let mut project = root.clone();
    for depth in 0..6 {
        project = project.join(format!("{depth}{}", "d".repeat(49)));
    }
    let irregular = project.join(".grapt").join("grapt.json");
    std::fs::create_dir_all(&irregular).map_err(|error| error.to_string())?;
    let not_file = CatalogSources::read(&project, CatalogEnvironment::isolated(&root)).err();
    std::fs::remove_dir(&irregular).map_err(|error| error.to_string())?;
    let malformed = project.join("grapt.jsonc");
    write(&malformed, r#"["secret-value"]"#)?;
    let not_object = CatalogSources::read(&project, CatalogEnvironment::isolated(&root)).err();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        irregular.to_string_lossy().len() > 300,
        "the fixture path is long"
    );
    let notice = |refusal: Option<String>| -> Result<String, String> {
        let refusal = refusal.ok_or("an unreadable source was accepted")?;
        Ok(grapt_session::notice::Unavailable::new(
            "plugins",
            &format!("they could not be refreshed: {refusal}"),
        )
        .reason)
    };
    let (not_file, not_object) = (notice(not_file)?, notice(not_object)?);
    assert_ne!(not_file, not_object, "two documents share one notice");
    // Reason first, then the path with its middle elided: the file and its
    // nearest directories survive whole, so nothing was cut at the bound.
    assert!(
        not_file.contains("catalog configuration is not a regular file: …/")
            && not_file.ends_with("/.grapt/grapt.json"),
        "{not_file}"
    );
    assert!(
        not_object.contains("catalog configuration root is not an object: …/")
            && not_object.ends_with("/grapt.jsonc"),
        "{not_object}"
    );
    assert!(!not_object.contains("secret-value"), "{not_object}");
    Ok(())
}

/// The review's long diagnostic: a legal 15-level nesting with
/// an invalid leaf yields a long parser diagnostic; under a long directory
/// the reason detail is cut, never the file name, inside a notice.
#[test]
fn a_long_parser_diagnostic_keeps_the_file_name_in_a_notice() -> Result<(), String> {
    let root = scratch("nested-decode");
    let mut project = root.clone();
    for depth in 0..6 {
        project = project.join(format!("{depth}{}", "d".repeat(49)));
    }
    let path = project.join("grapt.jsonc");
    let mut document = "?".to_owned();
    for _ in 0..15 {
        document = format!("[{}{document}]", "0,".repeat(500));
    }
    write(&path, &document)?;
    let refused = CatalogSources::read(&project, CatalogEnvironment::isolated(&root)).err();
    let _ = std::fs::remove_dir_all(&root);
    let refused = refused.ok_or("an undecodable source was accepted")?;
    let notice = grapt_session::notice::Unavailable::new(
        "plugins",
        &format!("they could not be refreshed: {refused}"),
    );
    assert!(notice.reason.ends_with("/grapt.jsonc"), "{}", notice.reason);
    assert!(
        notice.reason.len() < 256,
        "the notice was cut: {}",
        notice.reason
    );
    Ok(())
}

/// The closing rule, property-style: across long and
/// multibyte reasons, long and multibyte paths and every prefix up to the
/// budget, a notice is at most 256 bytes, is never cut by the notice bound,
/// ends with the exact file name, and keeps at least the reason floor.
#[test]
fn every_refusal_fits_a_notice_with_its_file_name_whole() {
    let reasons: Vec<String> = [0usize, 1, 39, 40, 41, 100, 249, 400]
        .iter()
        .flat_map(|&len| ["r", "é", "😀"].map(|unit| unit.repeat(len / unit.len().max(1))))
        .collect();
    let deep = |unit: &str| {
        (0..8).fold(PathBuf::from("/tmp"), |path, depth| {
            path.join(format!("{depth}{}", unit.repeat(40 / unit.len())))
        })
    };
    let paths = [
        PathBuf::from("/tmp/p/grapt.json"),
        deep("d").join(".grapt").join("grapt.json"),
        deep("é").join("grapt.jsonc"),
        PathBuf::from("/")
            .join(format!("{}x", "d".repeat(300)))
            .join("grapt.jsonc"),
        PathBuf::from("GRAPT_CONFIG_CONTENT"),
        PathBuf::from("/tmp/explicit").join(format!("{}A.json", "d".repeat(249))),
    ];
    for prefix_len in [0, 16, NOTICE_PREFIX_BYTES] {
        let prefix = "p".repeat(prefix_len);
        for reason in &reasons {
            for path in &paths {
                let refusal = refusal_text(reason, path);
                let text = format!("{prefix}{refusal}");
                let file = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                assert!(text.len() <= 256, "{} bytes: {text}", text.len());
                if file.len() <= 96 {
                    assert!(text.ends_with(&file), "{text}");
                } else {
                    let (shown, hash) = text.rsplit_once('#').unwrap_or_default();
                    assert!(
                        hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                        "{text}"
                    );
                    assert!(shown.ends_with(&file[file.len() - 40..]), "{text}");
                }
                assert_eq!(
                    grapt_session::notice::Unavailable::new("plugins", &text).reason,
                    text,
                    "the notice cut the refusal"
                );
                if !refusal.starts_with(reason.as_str()) {
                    let kept = refusal.split('…').next().unwrap_or_default();
                    assert!(
                        reason.starts_with(kept) && kept.len() + "…".len() + 3 >= 40,
                        "the reason fell below its floor: {refusal}"
                    );
                }
            }
        }
    }
}

/// The review's oversized explicit names:
/// legal 255-byte file names stay distinct inside a 256-byte notice with a
/// useful reason, whether they differ at the tail or only in the middle.
#[test]
fn oversized_explicit_names_stay_distinct_in_a_notice() {
    let reason = "catalog configuration is not a regular file";
    let notice = |name: String| {
        let path = PathBuf::from("/tmp/explicit").join(name);
        grapt_session::notice::Unavailable::new(
            "plugins",
            &format!(
                "they could not be refreshed: {}",
                refusal_text(reason, &path)
            ),
        )
        .reason
    };
    let pairs = [
        (
            format!("{}A.json", "d".repeat(249)),
            format!("{}B.json", "d".repeat(249)),
        ),
        // Same first and last 40 bytes: only the path hash tells them apart.
        (
            format!("{}X{}.json", "d".repeat(100), "d".repeat(149)),
            format!("{}Y{}.json", "d".repeat(100), "d".repeat(149)),
        ),
    ];
    let notices: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(one, two)| {
            assert_eq!(one.len(), 255);
            (notice(one), notice(two))
        })
        .collect();
    // Identity first: the hash is what keeps the middle-differing pair apart.
    for (one, two) in &notices {
        assert_ne!(one, two, "two oversized names share one notice");
    }
    for text in notices.iter().flat_map(|(one, two)| [one, two]) {
        assert!(text.len() <= 256, "{text}");
        assert!(text.contains(reason), "the reason was lost: {text}");
        assert!(
            text.contains(".json#"),
            "the extension left the tail: {text}"
        );
    }
}
