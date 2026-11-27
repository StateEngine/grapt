//! The service registration version precedence corpus (R4D-2V).
//!
//! The law table lives in `fixtures/version-precedence.toml`, outside the code
//! that implements it, so a reviewer can corrupt the FIXTURE and watch the
//! witness fail rather than only mutating the parser.
//!
//! Three properties keep this from passing for the wrong reason:
//!
//! 1. every section has an entry in [`CHECKS`], whose declared length is a
//!    compile-time bound — a section cannot be dropped from the run silently;
//! 2. each section asserts its exact row count, so a truncated corpus is a
//!    failure rather than a smaller pass; and
//! 3. a section that executed zero rows fails, because an empty section is not
//!    a passing section.

use std::collections::BTreeSet;
use std::error::Error;

use grapt_types::{CoreField, ServiceRegistrationVersion, UpgradeDisposition, VersionOrderError};

const CORPUS: &str = include_str!("fixtures/version-precedence.toml");

const EXPECTED_ASCENDING: usize = 22;
const EXPECTED_EQUAL_PRECEDENCE: usize = 4;
const EXPECTED_UNORDERABLE: usize = 10;

type Check = fn(&toml::Value) -> Result<usize, Box<dyn Error>>;

/// One checker per corpus section. The array's declared length is what binds
/// the owner to the fixture: removing a checker stops this file compiling
/// instead of quietly shrinking the run.
const CHECKS: [(&str, Check); 4] = [
    ("identity", check_identity),
    ("ascending", check_ascending),
    ("equal_precedence", check_equal_precedence),
    ("unorderable", check_unorderable),
];

#[test]
fn the_version_precedence_corpus_holds() -> Result<(), Box<dyn Error>> {
    let document: toml::Value = CORPUS.parse()?;
    let mut executed = 0usize;
    for (name, check) in CHECKS {
        let rows = check(&document).map_err(|error| format!("section {name}: {error}"))?;
        assert!(
            rows > 0,
            "section {name} executed no rows; an empty section is not a passing section"
        );
        executed += rows;
    }
    assert_eq!(
        executed,
        1 + EXPECTED_ASCENDING + EXPECTED_EQUAL_PRECEDENCE + EXPECTED_UNORDERABLE,
        "the corpus lost or gained rows between its sections and this owner"
    );
    Ok(())
}

fn check_identity(document: &toml::Value) -> Result<usize, Box<dyn Error>> {
    let owner = document
        .get("owner")
        .and_then(toml::Value::as_str)
        .ok_or("the corpus names no owner")?;
    assert_eq!(
        owner, "grapt-types::service::ServiceRegistrationVersion",
        "the corpus must name the type whose law it carries"
    );
    let schema_version = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .ok_or("the corpus declares no schema_version")?;
    assert_eq!(schema_version, 1, "unknown corpus schema version");
    Ok(1)
}

fn check_ascending(document: &toml::Value) -> Result<usize, Box<dyn Error>> {
    let rows = strings(document, "ascending")?;
    assert_eq!(
        rows.len(),
        EXPECTED_ASCENDING,
        "the ascending chain lost or gained rows"
    );

    let mut ordered = Vec::new();
    for raw in &rows {
        ordered.push((*raw, version(raw)?.semantic_precedence()?));
    }

    // Every pair, not only the adjacent ones: the chain asserts transitivity as
    // well as each step.
    for (index, (lower_raw, lower)) in ordered.iter().enumerate() {
        for (higher_raw, higher) in ordered.iter().skip(index + 1) {
            assert!(
                lower < higher,
                "{lower_raw} must precede {higher_raw} in semantic precedence"
            );
        }
    }
    Ok(rows.len())
}

fn check_equal_precedence(document: &toml::Value) -> Result<usize, Box<dyn Error>> {
    let rows = document
        .get("equal_precedence")
        .and_then(toml::Value::as_array)
        .ok_or("the corpus declares no equal_precedence section")?;
    assert_eq!(
        rows.len(),
        EXPECTED_EQUAL_PRECEDENCE,
        "the equal-precedence section lost or gained rows"
    );

    let mut disagreements = 0usize;
    for row in rows {
        let left_raw = field(row, "left")?;
        let right_raw = field(row, "right")?;
        let exactly_equal = row
            .get("exactly_equal")
            .and_then(toml::Value::as_bool)
            .ok_or("row declares no exactly_equal")?;

        let left = version(left_raw)?;
        let right = version(right_raw)?;

        assert_eq!(
            left.semantic_precedence()?,
            right.semantic_precedence()?,
            "{left_raw} and {right_raw} must have equal precedence"
        );
        assert_eq!(
            ServiceRegistrationVersion::upgrade_disposition(&left, &right)?,
            UpgradeDisposition::SamePrecedence,
            "equal precedence is never an upgrade, so {right_raw} cannot upgrade {left_raw}"
        );
        assert_eq!(
            left.is_exactly(&right),
            exactly_equal,
            "{left_raw} vs {right_raw}: exact-equality compatibility disagrees with the corpus"
        );

        if !exactly_equal {
            disagreements += 1;
        }
    }

    assert!(
        disagreements > 0,
        "the corpus must hold at least one pair whose precedence and exact-equality answers \
         DIFFER, or this section witnesses nothing about keeping the two rules apart"
    );
    Ok(rows.len())
}

fn check_unorderable(document: &toml::Value) -> Result<usize, Box<dyn Error>> {
    let rows = document
        .get("unorderable")
        .and_then(toml::Value::as_array)
        .ok_or("the corpus declares no unorderable section")?;
    assert_eq!(
        rows.len(),
        EXPECTED_UNORDERABLE,
        "the unorderable section lost or gained rows"
    );

    let mut families = BTreeSet::new();
    for row in rows {
        let value = field(row, "value")?;
        let reason = field(row, "reason")?;
        let detail = field(row, "detail")?;

        let stored = version(value)?;
        assert_eq!(
            stored.semantic_precedence(),
            Err(expected_refusal(reason, detail)?),
            "{value} must be refused for its exact reason, not merely refused"
        );
        assert_eq!(
            stored.as_str(),
            value,
            "an unorderable value is still recorded verbatim"
        );
        families.insert(reason.to_owned());
    }

    let expected_families: BTreeSet<String> = [
        "InvalidBuild",
        "InvalidCoreField",
        "InvalidPreRelease",
        "MalformedCore",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        families, expected_families,
        "the corpus must exercise every current refusal family exactly"
    );
    Ok(rows.len())
}

fn expected_refusal(reason: &str, detail: &str) -> Result<VersionOrderError, Box<dyn Error>> {
    Ok(match reason {
        "MalformedCore" => VersionOrderError::MalformedCore {
            fields: detail.parse()?,
        },
        "InvalidCoreField" => VersionOrderError::InvalidCoreField {
            field: core_field(detail)?,
        },
        "InvalidPreRelease" => VersionOrderError::InvalidPreRelease {
            at: detail.parse()?,
        },
        "InvalidBuild" => VersionOrderError::InvalidBuild {
            at: detail.parse()?,
        },
        // An unrecognised reason is evidence this owner did not understand the
        // corpus. It fails rather than skipping the row.
        other => return Err(format!("the corpus names an unknown refusal {other}").into()),
    })
}

fn core_field(detail: &str) -> Result<CoreField, Box<dyn Error>> {
    Ok(match detail {
        "major" => CoreField::Major,
        "minor" => CoreField::Minor,
        "patch" => CoreField::Patch,
        other => return Err(format!("the corpus names an unknown core field {other}").into()),
    })
}

fn version(raw: &str) -> Result<ServiceRegistrationVersion, Box<dyn Error>> {
    ServiceRegistrationVersion::new(raw)
        .ok_or_else(|| format!("{raw} is not a storable registration version").into())
}

fn field<'a>(row: &'a toml::Value, key: &str) -> Result<&'a str, Box<dyn Error>> {
    row.get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("row declares no {key}").into())
}

fn strings<'a>(document: &'a toml::Value, key: &str) -> Result<Vec<&'a str>, Box<dyn Error>> {
    let array = document
        .get(key)
        .and_then(toml::Value::as_array)
        .ok_or_else(|| format!("the corpus declares no {key} section"))?;
    let mut values = Vec::new();
    for entry in array {
        let value = entry
            .as_str()
            .ok_or_else(|| format!("{key} holds a non-string entry"))?;
        values.push(value);
    }
    Ok(values)
}
