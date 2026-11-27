//! Existing project cache buckets survive the product rename.
use grapt_provider::PromptCacheKey;

#[test]
fn project_prompt_cache_keeps_the_pre_rename_digest() {
    // Literal digests from the pre-rename NUL-delimited hash domain.
    for (project, provider, model, expected) in [
        (
            "project-fixture",
            "openai",
            "model-fixture",
            "4e2df0da025db0af621c4a9be73550172e7a68d80f01fd6042332077e2700f20",
        ),
        (
            "project-fixture",
            "anthropic",
            "model-fixture",
            "e7c8dc1bb77db0b5e72cd09d6309c18a60e4ae323bdb4283fdbbfbad70d7b704",
        ),
    ] {
        assert_eq!(
            PromptCacheKey::for_project(project, provider, model).as_str(),
            expected
        );
    }
}
