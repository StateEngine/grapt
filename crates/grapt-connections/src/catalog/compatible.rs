//! Explicit compatible replay contracts, independent of endpoint addresses.
use grapt_types::ProviderId;

pub fn requires_reasoning_content_replay(provider: &ProviderId) -> bool {
    provider.as_str() == "deepseek"
}
