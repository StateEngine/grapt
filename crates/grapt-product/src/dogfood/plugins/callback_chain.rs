//! One plugin callback chain: each callback's result feeds the next, and a
//! failed callback either stops the chain or is skipped with a warning.
use serde_json::Value;

/// What a failed plugin callback does to the chain it is part of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OnCallbackFailure {
    /// Skip the failing callback with a warning and go on.
    Skip,
    /// Stop: the failure is the outcome, as when a callback gates one call.
    Refuse,
}

/// Runs `callbacks` over `value` through `invoke`. A result that fails `check`
/// counts as that callback's failure. A skipped callback leaves the value it
/// was given, and its warning is added to `skipped`. With no callback applied
/// the result is `None`: nothing changed, so there is nothing to apply.
pub(super) fn run(
    callbacks: Vec<(String, u64)>,
    (domain, name): (&str, &str),
    mut value: Value,
    (on_failure, skipped): (OnCallbackFailure, &mut Vec<String>),
    check: &dyn Fn(&Value) -> Result<(), String>,
    mut invoke: impl FnMut(&str, u64, Value) -> Result<Value, String>,
) -> Result<Option<Value>, String> {
    let mut applied = false;
    for (plugin_id, callback) in callbacks {
        let next = invoke(&plugin_id, callback, value.clone())
            .and_then(|next| check(&next).map(|()| next));
        match next {
            Ok(next) => {
                value = next;
                applied = true;
            }
            Err(error) if on_failure == OnCallbackFailure::Refuse => {
                return Err(format!(
                    "plugin {plugin_id} {domain}.{name} failed: {error}"
                ));
            }
            Err(error) => {
                let warning =
                    format!("plugin {plugin_id} {domain}.{name} failed and was skipped: {error}");
                crate::diagnostics::report(warning.clone());
                skipped.push(warning);
            }
        }
    }
    Ok(applied.then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `a` appends, `b` fails or returns a malformed value, `c` appends.
    fn chain(
        on_failure: OnCallbackFailure,
        malformed: bool,
    ) -> (Result<Option<Value>, String>, usize) {
        let callbacks = ["a", "b", "c"].map(|id| (id.to_owned(), 0)).to_vec();
        let mut skipped = Vec::new();
        let result = run(
            callbacks,
            ("tool", "hook.execute.before"),
            json!(["start"]),
            (on_failure, &mut skipped),
            &|value| {
                value
                    .is_array()
                    .then_some(())
                    .ok_or_else(|| "malformed".into())
            },
            |plugin, _, mut value| match (plugin, malformed) {
                ("b", false) => Err("callback threw".into()),
                ("b", true) => Ok(json!({"not": "a list"})),
                _ => {
                    if let Some(list) = value.as_array_mut() {
                        list.push(json!(plugin));
                    }
                    Ok(value)
                }
            },
        );
        (result, skipped.len())
    }

    #[test]
    fn a_gating_chain_refuses_on_the_first_failure() {
        for malformed in [false, true] {
            let (result, skipped) = chain(OnCallbackFailure::Refuse, malformed);
            assert!(result.is_err_and(|error| error.contains("plugin b")));
            assert_eq!(skipped, 0);
        }
    }

    #[test]
    fn a_skipping_chain_keeps_the_value_before_a_failed_or_malformed_callback() {
        for malformed in [false, true] {
            let (result, skipped) = chain(OnCallbackFailure::Skip, malformed);
            assert_eq!(result, Ok(Some(json!(["start", "a", "c"]))));
            assert_eq!(skipped, 1);
        }
    }

    /// A chain whose every callback was skipped changed nothing.
    #[test]
    fn a_chain_with_no_applied_callback_has_no_result() {
        let mut skipped = Vec::new();
        let result = run(
            vec![("a".to_owned(), 0)],
            ("session", "hook.context"),
            json!({}),
            (OnCallbackFailure::Skip, &mut skipped),
            &|_| Err("malformed".into()),
            |_, _, value| Ok(value),
        );
        assert_eq!(result, Ok(None));
        assert_eq!(skipped.len(), 1);
    }
}
