//! Optional provider metadata cannot reject otherwise valid content.
use super::*;

fn additive_event(label: &str, route: Route) {
    let mut frames = vec![value!({"type":"future_additive_event","extension":true})];
    frames.extend(route.text(value!("ordinary answer")));
    let outcome = run(label, route, vec![route.completed(frames)]);
    assert!(
        outcome.completed(),
        "additive metadata rejected a valid completion: {}",
        outcome.history
    );
    assert_eq!(outcome.calls, 1);
    assert!(outcome.history.to_string().contains("ordinary answer"));
    outcome.assert_no_tools();
}

fn additive_fields(label: &str, route: Route) {
    let mut frames = route.text(value!("ordinary answer"));
    for frame in &mut frames {
        frame["future_extension"] = value!({"revision":2});
        match route {
            Route::Responses => (),
            Route::Compatible => {
                frame["choices"][0]["future_choice"] = value!(true);
                frame["choices"][0]["delta"]["future_delta"] = value!(true);
            }
            Route::Anthropic => {
                if frame.get("delta").is_some() {
                    frame["delta"]["future_delta"] = value!(true);
                }
            }
        }
    }
    let outcome = run(label, route, vec![route.completed(frames)]);
    assert!(outcome.completed(), "{}", outcome.history);
    assert_eq!(outcome.calls, 1);
    assert!(outcome.history.to_string().contains("ordinary answer"));
    outcome.assert_no_tools();
}

#[test]
fn responses_additive_unknown_event_preserves_valid_completion() {
    additive_event("responses-additive-event", Route::Responses);
}

#[test]
fn compatible_additive_unknown_event_preserves_valid_completion() {
    additive_event("compatible-additive-event", Route::Compatible);
}

#[test]
fn anthropic_additive_unknown_event_preserves_valid_completion() {
    additive_event("anthropic-additive-event", Route::Anthropic);
}

#[test]
fn responses_extra_fields_preserve_valid_completion() {
    additive_fields("responses-extra-fields", Route::Responses);
}

#[test]
fn compatible_extra_fields_preserve_valid_completion() {
    additive_fields("compatible-extra-fields", Route::Compatible);
}

#[test]
fn anthropic_extra_fields_preserve_valid_completion() {
    additive_fields("anthropic-extra-fields", Route::Anthropic);
}

fn malformed_usage(label: &str, route: Route) {
    let mut frames = route.text(value!("ordinary answer"));
    let script = match route {
        Route::Responses => {
            frames.extend([
                value!({"type":"response.output_text.done"}),
                value!({"type":"response.completed","response":{"usage":{
                    "input_tokens":"unknown","output_tokens":1,"total_tokens":2}}}),
            ]);
            raw(frames)
        }
        Route::Compatible => {
            frames.push(value!({"choices":[],"usage":{"prompt_tokens":"unknown"}}));
            route.completed(frames)
        }
        Route::Anthropic => {
            frames.push(value!({"type":"message_delta","usage":{"output_tokens":"unknown"}}));
            route.completed(frames)
        }
    };
    let outcome = run(label, route, vec![script]);
    assert!(
        outcome.completed(),
        "optional usage failed content: {}",
        outcome.history
    );
    assert_eq!(outcome.calls, 1);
    assert!(outcome.history.to_string().contains("ordinary answer"));
    outcome.assert_no_tools();
}

#[test]
fn responses_malformed_optional_usage_preserves_completion() {
    malformed_usage("responses-malformed-usage", Route::Responses);
}

#[test]
fn compatible_malformed_optional_usage_preserves_completion() {
    malformed_usage("compatible-malformed-usage", Route::Compatible);
}

#[test]
fn anthropic_malformed_optional_usage_preserves_completion() {
    malformed_usage("anthropic-malformed-usage", Route::Anthropic);
}
