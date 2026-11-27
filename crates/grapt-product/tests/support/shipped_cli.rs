//! Shipped-CLI journeys require a separately built sibling of the product.
//! Panic before launching the CLI so normal fixture cleanup still runs.

use std::path::{Path, PathBuf};
use std::sync::Once;

static MISSING_DIAGNOSTIC: Once = Once::new();

/// Cross-built native journeys stage this exact candidate beside their CLI.
/// Ordinary Cargo journeys keep Cargo's own product path.
pub(crate) fn product_binary() -> PathBuf {
    let path = std::env::var_os("GRAPT_HEADLESS_PRODUCT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_grapt-server")));
    assert!(
        path.is_absolute(),
        "fixture product path must be absolute: {}",
        path.display()
    );
    path
}

pub(crate) fn cli_binary() -> PathBuf {
    require_shipped_cli(&product_binary(), &MISSING_DIAGNOSTIC)
}

fn require_shipped_cli(product: &Path, diagnostic: &Once) -> PathBuf {
    let cli = product
        .with_file_name("grapt")
        .with_extension(std::env::consts::EXE_EXTENSION);
    let reason = match std::fs::metadata(&cli) {
        Ok(metadata) if metadata.is_file() => return cli,
        Ok(_) => "not a regular file".to_owned(),
        Err(error) => error.to_string(),
    };
    let mut first = false;
    // Panic outside Once: poisoning it would replace the next caller's
    // actionable diagnostic with an unrelated synchronization failure.
    diagnostic.call_once(|| first = true);
    if !first {
        panic!("shipped CLI missing; see the first failure (cargo build -p grapt-cli)");
    }
    let artifact_profile = cli
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let profile = if artifact_profile == "debug" {
        "dev"
    } else {
        &artifact_profile
    };
    panic!(
        "shipped CLI prerequisite failed: {} ({reason}); run `cargo build -p grapt-cli` \
         with the same target directory and profile `{profile}` (artifacts: `{artifact_profile}`)",
        cli.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_callers_get_build_instructions_and_present_callers_proceed() {
        let root = std::env::temp_dir().join(format!("grapt-cli-prereq-{}", std::process::id()));
        for artifact_profile in ["debug", "release"] {
            let directory = root.join("target").join(artifact_profile);
            std::fs::create_dir_all(&directory)
                .unwrap_or_else(|error| panic!("profile directory creation failed: {error}"));
            let product = directory.join("grapt-server");
            let cli = product
                .with_file_name("grapt")
                .with_extension(std::env::consts::EXE_EXTENSION);
            // No binary, a directory at the binary path, then a regular file.
            for case in 0..3 {
                match case {
                    1 => std::fs::create_dir(&cli).unwrap_or_else(|error| {
                        panic!("non-file fixture creation failed: {error}")
                    }),
                    2 => {
                        std::fs::remove_dir(&cli).unwrap_or_else(|error| {
                            panic!("non-file fixture removal failed: {error}")
                        });
                        std::fs::write(&cli, b"fixture executable")
                            .unwrap_or_else(|error| panic!("CLI fixture write failed: {error}"));
                    }
                    _ => {}
                }
                // Separate diagnostic state exercises the same precondition
                // without depending on other journeys' first-failure order.
                let diagnostic = Once::new();
                let outcomes = std::thread::scope(|scope| {
                    let callers: Vec<_> = (0..8)
                        .map(|_| {
                            scope.spawn(|| {
                                std::panic::catch_unwind(|| {
                                    require_shipped_cli(&product, &diagnostic)
                                })
                            })
                        })
                        .collect();
                    callers
                        .into_iter()
                        .map(|caller| {
                            caller
                                .join()
                                .unwrap_or_else(|_| panic!("prerequisite probe panicked"))
                        })
                        .collect::<Vec<_>>()
                });
                if case == 2 {
                    for outcome in outcomes {
                        assert_eq!(outcome.ok(), Some(cli.clone()), "present CLI was refused");
                    }
                    continue;
                }
                let messages: Vec<_> = outcomes
                    .into_iter()
                    .map(|outcome| {
                        let error = outcome
                            .err()
                            .unwrap_or_else(|| panic!("non-file CLI was accepted"));
                        error
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| {
                                error
                                    .downcast_ref::<&str>()
                                    .map(|message| (*message).to_owned())
                            })
                            .unwrap_or_else(|| panic!("non-text prerequisite failure"))
                    })
                    .collect();
                let full: Vec<_> = messages
                    .iter()
                    .filter(|message| message.starts_with("shipped CLI prerequisite failed:"))
                    .collect();
                assert_eq!(
                    full.len(),
                    1,
                    "first diagnostic repeated or missing: {messages:?}"
                );
                assert_eq!(messages.iter().filter(|message| {
                    message.as_str() == "shipped CLI missing; see the first failure (cargo build -p grapt-cli)"
                }).count(), 7, "later callers lost the build instruction: {messages:?}");
                let full = full[0];
                assert!(full.contains("cargo build -p grapt-cli"), "{full}");
                assert!(full.contains(cli.to_string_lossy().as_ref()), "{full}");
                let profile = if artifact_profile == "debug" {
                    "dev"
                } else {
                    artifact_profile
                };
                assert!(full.contains(&format!("profile `{profile}`")), "{full}");
                if case == 1 {
                    assert!(full.contains("not a regular file"), "{full}");
                }
            }
        }
        std::fs::remove_dir_all(&root)
            .unwrap_or_else(|error| panic!("prerequisite fixture cleanup failed: {error}"));
    }
}
