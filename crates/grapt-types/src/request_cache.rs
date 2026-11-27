//! Content-free request coordinates. Hashes describe wire equality, never cache hits.
/// Bound a durable request record independently of the request's input bound.
pub const MAX_PREFIX_ITEMS: usize = 512;
pub type RequestHash = [u8; 32];

/// Closed component set; authorization headers and credential bytes are excluded.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestComponents {
    pub target: RequestHash,
    pub cache_key: RequestHash,
    pub routing: RequestHash,
    pub tools: RequestHash,
    pub instructions: RequestHash,
    pub options: RequestHash,
    pub input: RequestHash,
}

impl std::fmt::Debug for RequestComponents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestComponents").finish_non_exhaustive()
    }
}

/// Each retained prefix digest covers every preceding input item as well.
/// The complete input digest distinguishes equal requests beyond the item cap.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestCacheFingerprint {
    components: RequestComponents,
    prefixes: Vec<RequestHash>,
    input_items: u32,
}

impl std::fmt::Debug for RequestCacheFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestCacheFingerprint")
            .finish_non_exhaustive()
    }
}
impl RequestCacheFingerprint {
    pub fn new(
        components: RequestComponents,
        prefixes: Vec<RequestHash>,
        input_items: u32,
    ) -> Option<Self> {
        if prefixes.len() != (input_items as usize).min(MAX_PREFIX_ITEMS) {
            return None;
        }
        Some(Self {
            components,
            prefixes,
            input_items,
        })
    }
    pub fn components(&self) -> &RequestComponents {
        &self.components
    }
    pub fn prefixes(&self) -> &[RequestHash] {
        &self.prefixes
    }
    pub fn input_items(&self) -> u32 {
        self.input_items
    }
    /// An item boundary, not a token count. A capped comparison is a lower bound.
    pub fn stable_items(&self, previous: &Self) -> usize {
        self.prefixes
            .iter()
            .zip(&previous.prefixes)
            .take_while(|(a, b)| a == b)
            .count()
    }
}

/// A bounded comparison result suitable for an accounting export.
#[derive(Debug, Clone)]
pub struct RequestCacheComparison {
    pub changed_components: Vec<&'static str>,
    pub stable_input_items: usize,
    pub stable_input_hash: Option<RequestHash>,
    pub complete: bool,
    pub append_only: bool,
    pub identical: bool,
}
impl RequestCacheFingerprint {
    pub fn compare(&self, previous: &Self) -> RequestCacheComparison {
        let a = &self.components;
        let b = &previous.components;
        let mut changed_components = Vec::new();
        for (name, changed) in [
            ("target", a.target != b.target),
            ("cacheKey", a.cache_key != b.cache_key),
            ("routing", a.routing != b.routing),
            ("tools", a.tools != b.tools),
            ("instructions", a.instructions != b.instructions),
            ("options", a.options != b.options),
        ] {
            if changed {
                changed_components.push(name);
            }
        }
        let matched = self.stable_items(previous);
        let different_item = matched < self.prefixes.len().min(previous.prefixes.len());
        let identical = a.input == b.input;
        let stable_input_items = if identical {
            self.input_items as usize
        } else {
            matched
        };
        if different_item || self.input_items < previous.input_items {
            changed_components.push("input");
        }
        RequestCacheComparison {
            stable_input_hash: if identical && self.input_items > 0 {
                Some(a.input)
            } else {
                matched.checked_sub(1).map(|i| self.prefixes[i])
            },
            complete: different_item
                || stable_input_items == (self.input_items.min(previous.input_items) as usize)
                || identical,
            append_only: stable_input_items == previous.input_items as usize
                && self.input_items > previous.input_items,
            identical,
            stable_input_items,
            changed_components,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fingerprint(items: u32, values: &[RequestHash]) -> Result<RequestCacheFingerprint, String> {
        RequestCacheFingerprint::new(
            RequestComponents {
                target: [0; 32],
                cache_key: [1; 32],
                routing: [2; 32],
                tools: [3; 32],
                instructions: [4; 32],
                options: [5; 32],
                input: [6; 32],
            },
            values.to_vec(),
            items,
        )
        .ok_or_else(|| "fingerprint".into())
    }
    #[test]
    fn exact_append_changed_components_and_capped_boundary_stay_distinct() -> Result<(), String> {
        let old = fingerprint(2, &[[8; 32], [9; 32]])?;
        let mut append = fingerprint(3, &[[8; 32], [9; 32], [10; 32]])?;
        append.components.input = [7; 32];
        let comparison = append.compare(&old);
        assert!(comparison.append_only && comparison.complete && !comparison.identical);
        assert_eq!(comparison.stable_input_items, 2);
        assert_eq!(comparison.stable_input_hash, Some([9; 32]));
        assert!(comparison.changed_components.is_empty());
        append.components.tools = [10; 32];
        append.components.instructions = [11; 32];
        assert_eq!(
            append.compare(&old).changed_components,
            vec!["tools", "instructions"]
        );
        let old = fingerprint(600, &vec![[8; 32]; MAX_PREFIX_ITEMS])?;
        assert_eq!(old.compare(&old).stable_input_items, 600);
        assert_eq!(old.compare(&old).stable_input_hash, Some([6; 32]));
        let mut current = fingerprint(601, &vec![[8; 32]; MAX_PREFIX_ITEMS])?;
        current.components.input = [7; 32];
        let comparison = current.compare(&old);
        assert!(!comparison.complete && !comparison.append_only);
        assert_eq!(comparison.stable_input_items, MAX_PREFIX_ITEMS);
        assert!(fingerprint(600, &[[8; 32]]).is_err());
        Ok(())
    }
}
