//! Project active answers in field order; hidden drafts never become conditions.
use super::*;
use serde_json::Value;

pub(super) fn project(
    row: &FormRow,
    values: &BTreeMap<String, FormValue>,
) -> Result<(Vec<usize>, BTreeMap<String, FormValue>), String> {
    if row.fields.len() > MAX_FIELDS {
        return Err("Too many fields for R; question remains unresolved".into());
    }
    let mut visible = Vec::new();
    let mut active = BTreeMap::<String, FormValue>::new();
    for (index, field) in row.fields.iter().enumerate() {
        let mut shown = true;
        if let Some(conditions) = field.attributes.get("when") {
            let conditions = conditions
                .as_array()
                .filter(|conditions| conditions.len() <= MAX_FIELDS)
                .ok_or("Unsupported field conditions")?;
            for condition in conditions {
                let key = condition["key"]
                    .as_str()
                    .ok_or("Unsupported condition key")?;
                if !row.fields[..index].iter().any(|earlier| earlier.key == key) {
                    return Err("Condition must name an earlier field; question unresolved".into());
                }
                let expected = condition
                    .get("value")
                    .ok_or("Unsupported condition value")?;
                let operation = condition["op"]
                    .as_str()
                    .ok_or("Unsupported condition operator")?;
                if !matches!(operation, "eq" | "neq")
                    || !(expected.is_string() || expected.is_boolean() || expected.is_number())
                    || condition.as_object().is_none_or(|object| object.len() != 3)
                {
                    return Err("Unsupported field condition".into());
                }
                let matched = active.get(key).is_some_and(|value| {
                    let value = value.to_json();
                    let hit = match &value {
                        Value::Array(items) => items.contains(expected),
                        _ => value == *expected,
                    };
                    if operation == "eq" {
                        hit
                    } else {
                        !hit
                    }
                });
                shown &= matched;
            }
        }
        if shown {
            visible.push(index);
            if let Some(value) = values.get(&field.key) {
                active.insert(field.key.clone(), value.clone());
            }
        }
    }
    Ok((visible, active))
}

impl Draft {
    pub(super) fn visible_fields(&self) -> Vec<usize> {
        project(&self.row, &self.values)
            .map(|(visible, _)| visible)
            .unwrap_or_else(|_| (0..self.row.fields.len().min(MAX_FIELDS)).collect())
    }
}
