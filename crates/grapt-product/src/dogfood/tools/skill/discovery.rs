//! Metadata discovery uses the existing skill catalog and permission runner.
use serde_json::{json, Value};

const PAGE: usize = 8;

#[cfg(test)]
pub(super) fn page(
    skills: &[Value],
    query: Option<&str>,
    cursor: usize,
) -> Result<Vec<u8>, String> {
    page_with_warnings(skills, query, cursor, &[])
}

pub(super) fn page_with_warnings(
    skills: &[Value],
    query: Option<&str>,
    cursor: usize,
    warnings: &[String],
) -> Result<Vec<u8>, String> {
    let terms: Vec<String> = query
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(str::to_lowercase)
        .collect();
    let matching: Vec<_> = crate::dogfood::catalogs::offered(skills)
        .into_iter()
        .filter(|skill| {
            let text = format!("{} {} {}", skill.id, skill.name, skill.description).to_lowercase();
            terms.iter().all(|term| text.contains(term))
        })
        .collect();
    if cursor > matching.len() {
        return Err("skill cursor is outside the current results; restart discovery".to_owned());
    }
    let end = cursor.saturating_add(PAGE).min(matching.len());
    let rows: Vec<_> = matching[cursor..end]
        .iter()
        .map(|skill| {
            json!({
                "id": skill.id, "name": skill.name, "description": skill.description
            })
        })
        .collect();
    let mut response = json!({"skills":rows, "total_matches":matching.len(),
        "next_cursor":(end < matching.len()).then_some(end)});
    if matching.is_empty() {
        response["guidance"] = Value::String(super::recovery::missing(skills, query.unwrap_or("")));
    }
    if !warnings.is_empty() {
        response["warnings"] = json!(warnings);
    }
    serde_json::to_vec(&response).map_err(|_| "skill catalog response encoding failed".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_search_explains_zero_matches_and_offers_installed_ids() -> Result<(), String> {
        let skills =
            vec![json!({"id":"skill-creator", "description":"Create skills", "content":"PRIVATE"})];
        let response: Value = serde_json::from_slice(&page(&skills, Some("landing"), 0)?)
            .map_err(|error| error.to_string())?;
        assert_eq!(response["total_matches"], 0);
        let guidance = response["guidance"]
            .as_str()
            .ok_or("zero-match recovery missing")?;
        assert!(guidance.contains("skill-creator"));
        assert!(guidance.contains("repository helper script"));
        assert!(!response.to_string().contains("PRIVATE"));
        Ok(())
    }

    #[test]
    fn every_offered_skill_remains_discoverable_with_its_complete_description() -> Result<(), String>
    {
        let mut skills: Vec<_> = (0..21).rev().map(|index| json!({
            "id":format!("sample-{index:02}"), "name":format!("Sample {index}"),
            "description":format!("{} specific tail {index}", "Full description. ".repeat(60))
        })).collect();
        skills.push(json!({"id":"manual", "description":"manual only", "autoinvoke":false}));
        skills.push(json!({"id":"empty", "description":" "}));
        let mut cursor = 0;
        let mut ids = Vec::new();
        loop {
            let value: Value = serde_json::from_slice(&page(&skills, None, cursor)?)
                .map_err(|error| error.to_string())?;
            let rows = value["skills"].as_array().ok_or("skill rows missing")?;
            assert!(rows.len() <= PAGE);
            assert_eq!(value["total_matches"], 21);
            for row in rows {
                let original = skills
                    .iter()
                    .find(|skill| skill["id"] == row["id"])
                    .ok_or("discovery returned a foreign skill")?;
                assert_eq!(row["description"], original["description"]);
                ids.push(row["id"].as_str().ok_or("skill id missing")?.to_owned());
            }
            let Some(next) = value["next_cursor"].as_u64() else {
                break;
            };
            cursor = usize::try_from(next).map_err(|_| "cursor overflow")?;
        }
        assert_eq!(
            ids,
            (0..21)
                .map(|index| format!("sample-{index:02}"))
                .collect::<Vec<_>>()
        );
        let found: Value = serde_json::from_slice(&page(&skills, Some("TAIL 17"), 0)?)
            .map_err(|error| error.to_string())?;
        assert_eq!(found["skills"][0]["id"], "sample-17");
        assert_eq!(found["total_matches"], 1);
        assert!(page(&skills, None, 22).is_err());
        Ok(())
    }
}
