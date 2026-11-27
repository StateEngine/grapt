//! Standard context windows; extended windows require an explicit override.
use serde_json::Value;

const FRESH_MS: i64 = 300_000;

fn bundled(model: &str) -> Option<(u64, u64)> {
    Some(match model {
        "gpt-6.1-terra" => (500_000, 872_000),
        "gpt-daybreak-red-latest" => (372_000, 372_000),
        "gpt-5.5" => (272_000, 272_000),
        "gpt-6-astra"
        | "gpt-6.1-sol"
        | "gpt-6-sol"
        | "gpt-6-luna"
        | "gpt-5.6-sol"
        | "gpt-5.6-terra"
        | "gpt-5.6-luna"
        | "gpt-daybreak-blue-latest"
        | "codex-auto-review" => (272_000, 872_000),
        _ => return None,
    })
}

fn positive(value: Option<&Value>) -> Result<Option<u64>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .filter(|value| value.checked_mul(100).is_some())
            .map(Some)
            .ok_or_else(|| "ChatGPT context window must be a bounded positive integer".into()),
    }
}

pub(super) fn prepare(
    listing: &mut Value,
    now_ms: i64,
    requested: Option<u64>,
) -> Result<(), String> {
    let fresh = listing
        .get("secode_fetched_at_ms")
        .and_then(Value::as_i64)
        .and_then(|fetched| now_ms.checked_sub(fetched))
        .is_some_and(|age| (0..=FRESH_MS).contains(&age));
    let rows = if listing.is_array() {
        listing.as_array_mut()
    } else {
        listing.get_mut("models").and_then(Value::as_array_mut)
    }
    .ok_or("ChatGPT discovery must contain a model array")?;
    if rows.len() > super::MAX_ACCOUNT_MODELS {
        return Err("account model count exceeds its bound".into());
    }
    for row in rows {
        // Hidden entries need no runnable capacity, as in the inventory owner.
        if row
            .get("visibility")
            .is_some_and(|value| !matches!(value.as_str(), Some("list" | "none")))
        {
            continue;
        }
        let model = row
            .get("slug")
            .or_else(|| row.get("id"))
            .and_then(Value::as_str)
            .ok_or("account model has no identity")?;
        let fallback = bundled(model);
        let declared = positive(row.get("context_window"))?;
        let declared_max = positive(row.get("max_context_window"))?;
        let standard = if fresh {
            declared.or(fallback.map(|value| value.0))
        } else {
            fallback.map(|value| value.0).or(declared)
        };
        let maximum = if fresh {
            declared_max.or(fallback.map(|value| value.1))
        } else {
            fallback.map(|value| value.1).or(declared_max)
        }
        .or(standard);
        let context = match requested {
            Some(value) => {
                value.min(maximum.ok_or("ChatGPT model has no declared maximum for an override")?)
            }
            None => standard.ok_or("ChatGPT model has no declared standard context window")?,
        };
        let fields = row
            .as_object_mut()
            .ok_or("account model is not an object")?;
        fields.insert("context_window".into(), context.into());
    }
    Ok(())
}
