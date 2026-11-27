use super::*;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repository(PathBuf);
impl Repository {
    fn new() -> TestResult<Self> {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("grapt-mention-files-{}-{id}", std::process::id()));
        // Never reuse or clean an existing directory left by another process.
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn put(&self, name: &str, text: &str) -> TestResult {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }
}
impl Drop for Repository {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}
fn ready(outcome: SearchOutcome) -> TestResult<SearchResults> {
    match outcome {
        SearchOutcome::Ready(results) => Ok(results),
        SearchOutcome::Cancelled => Err("unexpected cancellation".into()),
    }
}
fn paths(results: &SearchResults) -> BTreeSet<PathBuf> {
    results
        .matches
        .iter()
        .map(|row| row.relative_path.clone())
        .collect()
}
#[test]
fn repository_ignore_negation_nested_rules_hidden_files_and_git_metadata() -> TestResult {
    let repo = Repository::new()?;
    repo.put(".gitignore", "ignored/\n*.log\n!keep.log\n")?;
    for file in [
        "ignored/hidden.rs",
        "drop.log",
        "keep.log",
        ".hidden.txt",
        "src/main.rs",
        "nested/secret.txt",
        "nested/ok.rs",
        ".git/config",
    ] {
        repo.put(file, "fixture")?;
    }
    repo.put("nested/.gitignore", "secret.txt\n")?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::PRODUCTION)?;
    let results = ready(request.search()?)?;
    let found = paths(&results);
    for visible in ["keep.log", ".hidden.txt", "src/main.rs", "nested/ok.rs"] {
        assert!(
            found.contains(Path::new(visible)),
            "missing {visible}: {found:?}"
        );
    }
    for ignored in [
        "ignored/hidden.rs",
        "drop.log",
        "nested/secret.txt",
        ".git/config",
    ] {
        assert!(
            !found.contains(Path::new(ignored)),
            "offered ignored {ignored}"
        );
    }
    assert!(!results.entry_limit_reached);
    assert!(!results.depth_limit_reached);
    Ok(())
}
#[test]
fn fuzzy_ranking_uses_path_bonuses_and_keeps_only_the_best_matches() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "main.rs", Limits::new(2, 64, 16)?)?;
    let results = ready(
        request.rank_paths(
            [
                "scripts/many_helpers.rs",
                "src/main.rs",
                "main.rs",
                "README.md",
            ]
            .map(PathBuf::from),
        )?,
    )?;
    assert_eq!(results.matches.len(), 2);
    assert_eq!(results.matches[0].relative_path, Path::new("main.rs"));
    assert!(results.matches[0].score >= results.matches[1].score);
    assert_eq!(results.matches[1].relative_path, Path::new("src/main.rs"));
    let request = controller.begin(&repo.0, "cafe", Limits::PRODUCTION)?;
    let results = ready(request.rank_paths([PathBuf::from("src/café.rs")])?)?;
    assert_eq!(results.matches.len(), 1);
    Ok(())
}
#[test]
fn equal_scores_have_deterministic_path_order_independent_of_walk_order() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::new(2, 64, 16)?)?;
    let first = ready(request.rank_paths(["z.rs", "b.rs", "a.rs"].map(PathBuf::from))?)?;
    let second = ready(request.rank_paths(["a.rs", "z.rs", "a.rs", "b.rs"].map(PathBuf::from))?)?;
    assert_eq!(first.matches, second.matches);
    assert_eq!(
        first
            .matches
            .iter()
            .map(|row| row.relative_path.as_path())
            .collect::<Vec<_>>(),
        [Path::new("a.rs"), Path::new("b.rs")]
    );
    assert!(first.results_limited());
    Ok(())
}
#[test]
fn new_input_cancels_old_work_and_invalidates_already_finished_results() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let old = controller.begin(&repo.0, "", Limits::PRODUCTION)?;
    let old_results = ready(old.rank_paths([PathBuf::from("a.rs")])?)?;
    assert!(old_results.is_current());
    let next = controller.begin(&repo.0, "main", Limits::PRODUCTION)?;
    assert!(!old_results.is_current());
    assert!(matches!(old.search()?, SearchOutcome::Cancelled));
    let candidates = (0..32).map(|index| {
        if index == 3 {
            if let Err(error) = controller.begin(&repo.0, "new input", Limits::PRODUCTION) {
                panic!("new query: {error}");
            }
        }
        PathBuf::from(format!("main-{index}.rs"))
    });
    assert!(matches!(
        next.rank_paths(candidates)?,
        SearchOutcome::Cancelled
    ));
    assert!(!next.is_current());
    Ok(())
}
#[test]
fn invalid_input_and_controller_drop_cancel_without_filesystem_work() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let old = controller.begin(&repo.0, "old", Limits::PRODUCTION)?;
    assert!(matches!(
        controller.begin(&repo.0, "bad\nquery", Limits::PRODUCTION),
        Err(SearchError::Query)
    ));
    assert!(!old.is_current());
    assert!(matches!(
        controller.begin(
            &repo.0,
            &"x".repeat(MAX_QUERY_BYTES + 1),
            Limits::PRODUCTION
        ),
        Err(SearchError::Query)
    ));
    let pending = controller.begin(&repo.0, "pending", Limits::PRODUCTION)?;
    drop(controller);
    assert!(matches!(pending.search()?, SearchOutcome::Cancelled));
    Ok(())
}
#[test]
fn candidate_and_walk_work_bounds_are_visible_without_silently_scanning_forever() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::new(2, 5, 2)?)?;
    let mut visited = 0;
    let results = ready(request.rank_paths(std::iter::from_fn(|| {
        visited += 1;
        Some(PathBuf::from(format!("file-{visited}.rs")))
    }))?)?;
    assert_eq!(visited, 5);
    assert_eq!(results.scanned_entries, 5);
    assert_eq!(results.matches.len(), 2);
    assert!(results.entry_limit_reached);
    assert!(results.results_limited());
    repo.put("a/b/c/deep.rs", "deep")?;
    let request = controller.begin(&repo.0, "", Limits::new(16, 100, 2)?)?;
    let results = ready(request.search()?)?;
    assert!(results.depth_limit_reached);
    assert!(!paths(&results).contains(Path::new("a/b/c/deep.rs")));
    Ok(())
}
#[test]
fn invalid_paths_refuse_and_nonrepresentable_paths_are_counted() -> TestResult {
    let repo = Repository::new()?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::PRODUCTION)?;
    for invalid in [
        PathBuf::from("../outside"),
        repo.0.clone(),
        PathBuf::from(""),
    ] {
        assert!(matches!(
            request.rank_paths([invalid]),
            Err(SearchError::Candidate)
        ));
    }
    let results = ready(request.rank_paths([
        PathBuf::from("x".repeat(MAX_PATH_BYTES + 1)),
        PathBuf::from("ok.rs"),
    ])?)?;
    assert_eq!(results.omitted_paths, 1);
    assert_eq!(results.matches.len(), 1);
    assert!(Limits::new(0, 1, 1).is_err());
    assert!(Limits::new(101, 1, 1).is_err());
    assert!(Limits::new(1, 100_001, 1).is_err());
    assert!(Limits::new(1, 1, 65).is_err());
    let absent = controller.begin(&repo.0.join("absent"), "", Limits::PRODUCTION)?;
    assert!(matches!(absent.search(), Err(SearchError::RootRead(_))));
    Ok(())
}
#[cfg(unix)]
#[test]
fn symlinks_never_offer_files_outside_the_search_root() -> TestResult {
    use std::os::unix::fs::symlink;
    let repo = Repository::new()?;
    let outside = Repository::new()?;
    repo.put("local.rs", "local")?;
    outside.put("secret.rs", "outside")?;
    symlink(&outside.0, repo.0.join("linked-directory"))?;
    symlink(outside.0.join("secret.rs"), repo.0.join("linked-file.rs"))?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::PRODUCTION)?;
    let results = ready(request.search()?)?;
    assert_eq!(paths(&results), BTreeSet::from([PathBuf::from("local.rs")]));
    Ok(())
}

#[test]
fn repository_ignore_errors_are_not_a_successful_partial_search() -> TestResult {
    let repo = Repository::new()?;
    std::fs::create_dir(repo.0.join(".git"))?;
    repo.put(".gitignore", "[z-a]\n")?;
    repo.put("visible.rs", "visible")?;
    let mut controller = SearchController::default();
    let request = controller.begin(&repo.0, "", Limits::PRODUCTION)?;
    assert!(matches!(request.search(), Err(SearchError::Walk(_))));
    Ok(())
}

#[test]
fn parent_gitignore_stops_at_child_repository_boundary() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".gitignore", "*\n")?;
    parent.put("repo/.git/config", "")?;
    parent.put("repo/.gitignore", "*.log\n!keep.log\n")?;
    parent.put("repo/nested/.gitignore", "secret.txt\n")?;
    for file in [
        "visible.rs",
        "drop.log",
        "keep.log",
        "nested/secret.txt",
        "nested/ok.rs",
    ] {
        parent.put(&format!("repo/{file}"), "fixture")?;
    }
    let mut controller = SearchController::default();
    let request = controller.begin(&parent.0.join("repo"), "", Limits::PRODUCTION)?;
    let found = paths(&ready(request.search()?)?);
    for visible in ["visible.rs", "keep.log", "nested/ok.rs"] {
        assert!(
            found.contains(Path::new(visible)),
            "missing {visible}: {found:?}"
        );
    }
    for ignored in ["drop.log", "nested/secret.txt", ".git/config"] {
        assert!(
            !found.contains(Path::new(ignored)),
            "offered ignored {ignored}"
        );
    }
    Ok(())
}

#[test]
fn nonrepository_ignores_gitignore_but_honors_dot_ignore() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".gitignore", "*\n")?;
    parent.put("plain/.gitignore", "visible.rs\n")?;
    parent.put("plain/.ignore", "hidden.rs\n")?;
    parent.put("plain/visible.rs", "visible")?;
    parent.put("plain/hidden.rs", "ignored")?;
    let mut controller = SearchController::default();
    let request = controller.begin(&parent.0.join("plain"), "", Limits::PRODUCTION)?;
    let found = paths(&ready(request.search()?)?);
    assert!(found.contains(Path::new("visible.rs")));
    assert!(!found.contains(Path::new("hidden.rs")));
    Ok(())
}

#[test]
fn ceiling_ignores_parent_repository_but_keeps_nested_git_rules() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".git/config", "")?;
    parent.put(".gitignore", "*\n")?;
    parent.put("plain/.gitignore", "visible.rs\n")?;
    parent.put("plain/.ignore", "hidden.rs\n")?;
    parent.put("plain/visible.rs", "visible")?;
    parent.put("plain/hidden.rs", "hidden")?;
    parent.put("plain/nested/.git/config", "")?;
    parent.put("plain/nested/.gitignore", "*.log\n")?;
    parent.put("plain/nested/visible.rs", "visible")?;
    parent.put("plain/nested/hidden.log", "hidden")?;
    let mut controller = SearchController::default();
    let root = parent.0.join("plain");
    let request = controller.begin(&root, "", Limits::PRODUCTION)?;
    let rules = parents::Rules::below(&root, std::slice::from_ref(&parent.0))?;
    let found = paths(&ready(request.search_with_rules(rules)?)?);
    assert!(found.contains(Path::new("visible.rs")), "{found:?}");
    assert!(found.contains(Path::new("nested/visible.rs")), "{found:?}");
    assert!(!found.contains(Path::new("hidden.rs")), "{found:?}");
    assert!(!found.contains(Path::new("nested/hidden.log")), "{found:?}");
    Ok(())
}

#[test]
fn confined_nested_repository_cannot_reopen_an_enclosing_ignored_directory() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".git/config", "")?;
    parent.put("plain/outer/.git/config", "")?;
    parent.put("plain/outer/.gitignore", "ignored/\n")?;
    parent.put("plain/outer/visible.rs", "visible")?;
    parent.put("plain/outer/ignored/.git/config", "")?;
    parent.put("plain/outer/ignored/leaked.rs", "hidden")?;
    let root = parent.0.join("plain");
    let mut controller = SearchController::default();
    let request = controller.begin(&root, "", Limits::PRODUCTION)?;
    let rules = parents::Rules::below(&root, std::slice::from_ref(&parent.0))?;
    let found = paths(&ready(request.search_with_rules(rules)?)?);
    assert!(found.contains(Path::new("outer/visible.rs")), "{found:?}");
    assert!(
        !found.contains(Path::new("outer/ignored/leaked.rs")),
        "{found:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_ceiling_does_not_import_parent_ignore_rules() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".git/config", "")?;
    parent.put(".gitignore", "*\n")?;
    parent.put("plain/visible.rs", "visible")?;
    let alias = parent.0.join("alias");
    std::os::unix::fs::symlink(&parent.0, &alias)?;
    for (root, ceiling) in [
        (parent.0.join("plain"), &alias),
        (alias.join("plain"), &parent.0),
    ] {
        let mut controller = SearchController::default();
        let request = controller.begin(&root, "", Limits::PRODUCTION)?;
        let rules = parents::Rules::below(&root, std::slice::from_ref(ceiling))?;
        let found = paths(&ready(request.search_with_rules(rules)?)?);
        assert!(found.contains(Path::new("visible.rs")), "{found:?}");
    }
    Ok(())
}

#[test]
fn admitted_nested_repository_resets_outer_git_ignore_scope() -> TestResult {
    let parent = Repository::new()?;
    parent.put(".git/config", "")?;
    parent.put("plain/outer/.git/config", "")?;
    parent.put("plain/outer/.gitignore", "*.log\n")?;
    parent.put("plain/outer/drop.log", "hidden")?;
    parent.put("plain/outer/nested/.git/config", "")?;
    parent.put("plain/outer/nested/keep.log", "visible")?;
    let root = parent.0.join("plain");
    let mut controller = SearchController::default();
    let request = controller.begin(&root, "", Limits::PRODUCTION)?;
    let rules = parents::Rules::below(&root, std::slice::from_ref(&parent.0))?;
    let found = paths(&ready(request.search_with_rules(rules)?)?);
    assert!(!found.contains(Path::new("outer/drop.log")), "{found:?}");
    assert!(
        found.contains(Path::new("outer/nested/keep.log")),
        "{found:?}"
    );
    Ok(())
}
