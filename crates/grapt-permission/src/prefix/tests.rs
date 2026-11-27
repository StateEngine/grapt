use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn sec03_prefix_matches_exact_argument_positions_and_allows_trailing_arguments(
) -> Result<(), &'static str> {
    let pattern = PrefixPattern::new(vec![args(&["git"]), args(&["status", "diff"])])?;
    for command in [
        args(&["git", "status"]),
        args(&["git", "status", "--short"]),
        args(&["git", "diff", "--stat"]),
    ] {
        assert!(pattern.matches(&command), "{command:?}");
    }
    for command in [
        vec![],
        args(&["git"]),
        args(&["git", "push"]),
        args(&["git", "status-extra"]),
        args(&["git", "--no-pager", "status"]),
        args(&["/tmp/git", "status"]),
        args(&["Git", "status"]),
        args(&["git status"]),
    ] {
        assert!(
            !pattern.matches(&command),
            "literal execution prefix matched unrelated argv: {command:?}"
        );
    }
    Ok(())
}

#[test]
fn sec03_prefix_glob_characters_and_empty_arguments_are_literal() -> Result<(), &'static str> {
    let pattern = PrefixPattern::new(vec![args(&["echo"]), args(&["*", ""]), args(&["?"])])?;
    assert!(pattern.matches(&args(&["echo", "*", "?"])));
    assert!(pattern.matches(&args(&["echo", "", "?"])));
    assert!(!pattern.matches(&args(&["echo", "anything", "?"])));
    assert!(!pattern.matches(&args(&["echo", "*", "x"])));
    Ok(())
}

#[test]
fn sec03_prefix_refuses_ambiguous_or_excessive_patterns() -> Result<(), &'static str> {
    for positions in [
        vec![],
        vec![vec![]],
        vec![args(&[""])],
        vec![args(&["git", "hg"])],
        vec![args(&["git"]), vec![]],
        vec![args(&["git"]), args(&["bad\0argument"])],
        vec![args(&["git"]), vec!["x".repeat(4094)]],
        vec![args(&["git"]), vec!["x".into(); 65]],
        vec![args(&["x"]); 65],
    ] {
        assert!(PrefixPattern::new(positions).is_err());
    }
    assert!(
        PrefixPattern::new(vec![args(&["git"]), vec!["x".repeat(4093)]])?
            .matches(&["git".into(), "x".repeat(4093),])
    );
    assert!(PrefixPattern::new(vec![args(&["x"]); 64])?.matches(&vec!["x".into(); 64]));
    Ok(())
}
