//! MCP form schemas projected into the existing form vocabulary.
use serde_json::{json, Value};

/// Accept bounded primitive fields, refusing unsupported schema semantics.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn fields(params: &Value) -> Result<Value, String> {
    if params.get("mode").is_some_and(|mode| mode != "form") {
        return Err("this elicitation requires an unsupported mode".into());
    }
    let schema = params
        .get("requestedSchema")
        .ok_or("elicitation schema absent")?;
    if schema["type"] != "object" {
        return Err("elicitation schema must be an object".into());
    }
    for name in schema.as_object().ok_or("elicitation schema")?.keys() {
        if !["type", "properties", "required", "title", "description"].contains(&name.as_str()) {
            return Err(format!("unsupported elicitation schema keyword: {name}"));
        }
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("elicitation properties absent")?;
    if properties.len() > 64 {
        return Err("elicitation field bound exceeded".into());
    }
    let required = match schema.get("required") {
        None => Vec::new(),
        Some(value) => {
            let rows = value
                .as_array()
                .filter(|rows| rows.len() <= 64)
                .ok_or("elicitation required fields")?;
            let mut keys = Vec::new();
            for row in rows {
                let key = row
                    .as_str()
                    .filter(|key| properties.contains_key(*key))
                    .ok_or("unknown required field")?;
                if keys.contains(&key) {
                    return Err("repeated required field".into());
                }
                keys.push(key);
            }
            keys
        }
    };
    let mut fields = Vec::with_capacity(properties.len());
    for (key, property) in properties {
        if key.is_empty() || key.len() > 256 {
            return Err("elicitation field identity".into());
        }
        let kind = property["type"]
            .as_str()
            .filter(|kind| matches!(*kind, "string" | "boolean" | "number" | "integer" | "array"))
            .ok_or("unsupported elicitation field type")?;
        let mut field = json!({"key":key,"type":kind});
        if required.contains(&key.as_str()) {
            field["required"] = json!(true);
        }
        for name in [
            "title",
            "description",
            "default",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "format",
            "minItems",
            "maxItems",
        ] {
            if let Some(value) = property.get(name) {
                field[name] = value.clone();
            }
        }
        // Unknown validation keywords cannot silently become weaker validation.
        for name in property.as_object().ok_or("elicitation property")?.keys() {
            if ![
                "type",
                "title",
                "description",
                "default",
                "minimum",
                "maximum",
                "minLength",
                "maxLength",
                "format",
                "enum",
                "enumNames",
                "oneOf",
                "items",
                "minItems",
                "maxItems",
            ]
            .contains(&name.as_str())
            {
                return Err(format!("unsupported elicitation field keyword: {name}"));
            }
        }
        if kind == "array" {
            let items = property["items"]
                .as_object()
                .ok_or("elicitation array items")?;
            if items.get("type").is_some_and(|kind| kind != "string")
                || items
                    .keys()
                    .any(|key| !["type", "enum", "anyOf"].contains(&key.as_str()))
            {
                return Err("unsupported elicitation array item semantics".into());
            }
            field["type"] = json!("multiselect");
            field["options"] = selection_options(&property["items"], "anyOf")?;
            field["custom"] = json!(false);
            fields.push(field);
            continue;
        }
        if property.get("oneOf").is_some() {
            if kind != "string" {
                return Err("elicitation selection must contain strings".into());
            }
            field["options"] = selection_options(property, "oneOf")?;
            field["custom"] = json!(false);
        } else if let Some(options) = property.get("enum") {
            if kind != "string" {
                return Err("elicitation enum must contain strings".into());
            }
            let options = options
                .as_array()
                .filter(|options| !options.is_empty() && options.len() <= 128)
                .ok_or("elicitation enum bound")?;
            let names = property
                .get("enumNames")
                .map(|value| {
                    value
                        .as_array()
                        .filter(|names| names.len() == options.len())
                        .ok_or("elicitation enum labels")
                })
                .transpose()?;
            let mut mapped = Vec::with_capacity(options.len());
            for (index, option) in options.iter().enumerate() {
                let option = option.as_str().ok_or("elicitation enum value")?;
                let label = match names {
                    None => option,
                    Some(names) => names[index].as_str().ok_or("elicitation enum label")?,
                };
                mapped.push(json!({"value":option,"label":label}));
            }
            field["options"] = json!(mapped);
            field["custom"] = json!(false);
        } else if property.get("enumNames").is_some() {
            return Err("elicitation labels without enum".into());
        }
        fields.push(field);
    }
    let value = json!(fields);
    if !properties.is_empty() {
        if let Some(error) = crate::answer::validate_fields(&value) {
            return Err(error);
        }
    }
    Ok(value)
}

fn selection_options(schema: &Value, alternatives: &str) -> Result<Value, String> {
    let schema = schema.as_object().ok_or("elicitation selection schema")?;
    let rows = match (schema.get("enum"), schema.get(alternatives)) {
        (Some(rows), None) | (None, Some(rows)) => rows
            .as_array()
            .filter(|rows| !rows.is_empty() && rows.len() <= 128)
            .ok_or("elicitation selection bound")?,
        _ => return Err("elicitation selection must declare exactly one option set".into()),
    };
    let alternatives_used = schema.contains_key(alternatives);
    let mut options = Vec::with_capacity(rows.len());
    for row in rows {
        let (value, label) = if alternatives_used {
            let object = row.as_object().ok_or("elicitation selection option")?;
            if object
                .keys()
                .any(|key| !["const", "title"].contains(&key.as_str()))
            {
                return Err("unsupported elicitation selection option semantics".into());
            }
            let value = row["const"].as_str().ok_or("elicitation selection value")?;
            let label = row["title"].as_str().ok_or("elicitation selection label")?;
            (value, label)
        } else {
            let value = row.as_str().ok_or("elicitation selection value")?;
            (value, value)
        };
        options.push(json!({"value":value,"label":label}));
    }
    Ok(json!(options))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_validation_is_refused_instead_of_silently_discarded() {
        for property in [
            json!({"type":"array","items":{"type":"string","enum":["a"],"pattern":"b"}}),
            json!({"type":"array","items":{"type":"integer","enum":["a"]}}),
            json!({"type":"string","oneOf":[{"const":"a","title":"A","minLength":2}]}),
        ] {
            assert!(fields(
                &json!({"requestedSchema":{"type":"object","properties":{"choice":property}}})
            )
            .is_err());
        }
        assert!(fields(
            &json!({"requestedSchema":{"type":"object","properties":{},"minProperties":1}})
        )
        .is_err());
    }

    #[test]
    fn selection_and_numeric_constraints_reach_the_existing_answer_validator() -> Result<(), String>
    {
        let fields = fields(&json!({"requestedSchema":{"type":"object","properties":{
            "age":{"type":"integer","minimum":18},
            "choice":{"type":"string","oneOf":[{"const":"a","title":"A"}]},
            "many":{"type":"array","items":{"type":"string","enum":["a","b"]},"minItems":1,"maxItems":2}
        },"required":["age","choice","many"]}}))?;
        assert!(crate::answer::validate_answer(
            &fields,
            &json!({"age":18,"choice":"a","many":["a"]})
        )
        .is_none());
        for invalid in [
            json!({"age":17,"choice":"a","many":["a"]}),
            json!({"age":18,"choice":"b","many":["a"]}),
            json!({"age":18,"choice":"a","many":[]}),
        ] {
            assert!(crate::answer::validate_answer(&fields, &invalid).is_some());
        }
        Ok(())
    }
}
