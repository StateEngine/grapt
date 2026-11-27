use super::*;

fn rules(raw: &str) -> Result<Vec<PrefixRule>, String> {
    let document = grapt_config::decode(
        format!("{{\"execpolicy\":{raw}}}").as_bytes(),
        crate::dogfood::catalog_config::bounds()?,
    )
    .map_err(|error| error.to_string())?
    .into_value();
    let mut rules = Vec::new();
    parse(
        document.get("execpolicy").ok_or("execpolicy")?.value(),
        &mut rules,
    )?;
    Ok(rules)
}

fn command(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn sec03_execution_rules_use_literal_alternatives_and_the_strictest_match() -> Result<(), String> {
    let rules = rules(
        r#"[{"pattern":["git"],"decision":"allow"},
            {"pattern":["git",["status","diff"]],"decision":"ask"},
            {"pattern":["git","push"],"decision":"deny"}]"#,
    )?;
    for (words, expected) in [
        (command(&["git", "log"]), Effect::Allow),
        (command(&["git", "status", "--short"]), Effect::Ask),
        (command(&["git", "diff"]), Effect::Ask),
        (command(&["git", "push", "origin"]), Effect::Deny),
    ] {
        assert_eq!(decision(&rules, &[words]), Some(expected));
    }
    assert_eq!(decision(&rules, &[command(&["/tmp/git", "status"])]), None);
    assert_eq!(decision(&rules, &[]), None);
    Ok(())
}

#[test]
fn sec03_execution_allow_never_covers_a_partly_unmatched_compound() -> Result<(), String> {
    let rules = rules(
        r#"[{"pattern":["git","status"],"decision":"allow"},
            {"pattern":["cargo","test"],"decision":"ask"},
            {"pattern":["rm"],"decision":"deny"}]"#,
    )?;
    let allowed = command(&["git", "status"]);
    let unmatched = command(&["unknown"]);
    assert_eq!(
        decision(&rules, std::slice::from_ref(&allowed)),
        Some(Effect::Allow)
    );
    assert_eq!(
        decision(&rules, &[allowed.clone(), unmatched.clone()]),
        None
    );
    assert_eq!(
        decision(&rules, &[allowed.clone(), command(&["cargo", "test"])]),
        Some(Effect::Ask)
    );
    assert_eq!(
        decision(&rules, &[unmatched, command(&["rm", "file"]), allowed]),
        Some(Effect::Deny)
    );
    Ok(())
}

#[test]
fn sec03_execution_malformed_rules_refuse_without_echoing_the_value() {
    for raw in [
        "null",
        "{}",
        "[null]",
        r#"[{"pattern":["git"],"decision":"PRIVATE_INVALID"}]"#,
        r#"[{"pattern":[],"decision":"allow"}]"#,
        r#"[{"pattern":[["git","hg"]],"decision":"allow"}]"#,
        r#"[{"pattern":["git",[]],"decision":"allow"}]"#,
        r#"[{"pattern":["git",[1]],"decision":"allow"}]"#,
        r#"[{"pattern":["git"],"decision":"allow","unknown":"PRIVATE_INVALID"}]"#,
        r#"[{"pattern":["git"]}]"#,
        r#"[{"decision":"deny"}]"#,
    ] {
        let error = match rules(raw) {
            Ok(_) => panic!("malformed execution configuration was accepted"),
            Err(error) => error,
        };
        assert!(!error.contains("PRIVATE_INVALID"));
    }
}

#[test]
fn sec03_execution_decoder_enforces_rule_and_argument_bounds() -> Result<(), String> {
    let entry = r#"{"pattern":["git"],"decision":"allow"}"#;
    let document = |count| format!("[{}]", vec![entry; count].join(","));
    assert_eq!(rules(&document(1024))?.len(), 1024);
    assert!(rules(&document(1025)).is_err());

    for pattern in [
        serde_json::json!([""]),
        serde_json::json!(vec!["git"; 65]),
        serde_json::json!(["git", vec!["status"; 65]]),
        serde_json::json!(["git", "x".repeat(4094)]),
        serde_json::json!(["git", "private\0argument"]),
    ] {
        let value = serde_json::json!([{"pattern":pattern,"decision":"allow"}]);
        assert!(rules(&value.to_string()).is_err());
    }
    Ok(())
}
