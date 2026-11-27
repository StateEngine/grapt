//! Closed local request diagnostics, separate from provider cache usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestChange {
    FirstObservation,
    Target,
    CacheKey,
    Routing,
    Tools,
    Instructions,
    Options,
    Input { index: u16 },
    Shortened { index: u16 },
    AppendOnly,
    Identical,
    Bound,
}
impl RequestChange {
    pub(crate) fn description(self) -> String {
        match self {
            Self::FirstObservation => "no previous comparison".to_owned(),
            Self::Target => "target changed".to_owned(),
            Self::CacheKey => "cache key changed".to_owned(),
            Self::Routing => "cache routing changed".to_owned(),
            Self::Tools => "tools changed".to_owned(),
            Self::Instructions => "instructions changed".to_owned(),
            Self::Options => "options changed".to_owned(),
            Self::Input { index } => format!("input item {} changed", u32::from(index) + 1),
            Self::Shortened { index } => {
                format!("input shortened at item {}", u32::from(index) + 1)
            }
            Self::AppendOnly => "input appended".to_owned(),
            Self::Identical => "identical body".to_owned(),
            Self::Bound => "comparison bound reached".to_owned(),
        }
    }
}
