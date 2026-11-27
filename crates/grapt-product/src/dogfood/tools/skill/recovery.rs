//! Recovery uses offered catalog IDs only; it never loads skill content.
use serde_json::Value;

const PREFIX: &str = "Skill not found in the installed catalog. Nearest available skills: ";
const NEXT: &str = ". Use {} to list skills or query to search; use an exact installed id to load. A repository helper script is not automatically a skill; run its documented command instead.";
const SUGGESTIONS: usize = 3;
const ID_CHARS: usize = 96;

pub(super) fn missing(skills: &[Value], requested: &str) -> String {
    let mut ids: Vec<_> = crate::dogfood::catalogs::offered(skills)
        .into_iter()
        .map(|skill| skill.id)
        .filter(|id| valid_id(id))
        .map(|id| (distance(requested, &id), id))
        .collect();
    ids.sort();
    let ids: Vec<_> = ids
        .into_iter()
        .take(SUGGESTIONS)
        .map(|(_, id)| id)
        .collect();
    format!(
        "{PREFIX}{}{NEXT}",
        if ids.is_empty() {
            "(none)".to_owned()
        } else {
            ids.join(", ")
        }
    )
}

pub(in crate::dogfood) fn public_detail(detail: &str) -> Option<&str> {
    let ids = detail.strip_prefix(PREFIX)?.strip_suffix(NEXT)?;
    (ids == "(none)" || (ids.split(", ").count() <= SUGGESTIONS && ids.split(", ").all(valid_id)))
        .then_some(detail)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_CHARS
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '/'))
}

// Bounded edit distance for typo recovery; deterministic ties use catalog ID.
fn distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right
        .chars()
        .take(ID_CHARS)
        .flat_map(char::to_lowercase)
        .collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, left) in left
        .chars()
        .take(ID_CHARS)
        .flat_map(char::to_lowercase)
        .enumerate()
    {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(left != *right))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_skill_suggests_nearest_offered_ids_without_loading_content() {
        let skills = vec![
            json!({"id":"imagegen", "description":"Images", "content":"PRIVATE BODY"}),
            json!({"id":"skill-creator", "description":"Skills"}),
            json!({"id":"landing-manual", "description":"Manual", "autoinvoke":false}),
            json!({"id":"secret\nID", "description":"Invalid"}),
        ];
        let detail = missing(&skills, "skill-creatr");
        assert!(detail.starts_with(&format!("{PREFIX}skill-creator, imagegen")));
        assert_eq!(public_detail(&detail), Some(detail.as_str()));
        assert!(!detail.contains("PRIVATE") && !detail.contains("landing-manual"));
        let landing = missing(&skills, "landing");
        assert!(landing.contains("repository helper script is not automatically a skill"));
        assert_eq!(public_detail(&format!("{detail} PRIVATE")), None);
        assert!(missing(&[], "PRIVATE REQUEST").contains("(none)"));
        assert!(!missing(&[], "PRIVATE REQUEST").contains("PRIVATE"));
    }
}
