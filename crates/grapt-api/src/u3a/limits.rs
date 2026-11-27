use std::num::NonZeroUsize;

use thiserror::Error;

pub const MAX_REQUEST_DTO_BYTES: usize = 65_536;
pub const MAX_DTO_STRING_BYTES: usize = 262_144;
pub const MAX_NESTED_ITEMS: usize = 256;
pub const MAX_PAGE_BYTES: usize = 8_388_608;
pub const MAX_PROMPT_TEXT_BYTES: usize = 65_536;
pub const MAX_METADATA_BYTES: usize = 16_384;
pub const MAX_METADATA_DEPTH: usize = 8;
pub const MAX_METADATA_NODES: usize = 256;
pub const MAX_METADATA_KEYS: usize = 64;
pub const MAX_METADATA_KEY_BYTES: usize = 256;
pub const MAX_METADATA_SCALAR_BYTES: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct U3aLimits {
    pub(super) request_dto_bytes: NonZeroUsize,
    pub(super) nested_items: NonZeroUsize,
    pub(super) page_bytes: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct U3aLimitValues {
    pub request_dto_bytes: usize,
    pub nested_items: usize,
    pub page_bytes: usize,
}

pub const U3A_PRODUCTION_LIMITS: U3aLimitValues = U3aLimitValues {
    request_dto_bytes: MAX_REQUEST_DTO_BYTES,
    nested_items: MAX_NESTED_ITEMS,
    page_bytes: MAX_PAGE_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum U3aLimitsError {
    #[error("{field} must be positive")]
    Zero { field: &'static str },
    #[error("request DTO bytes may not exceed the 65,536-byte host body bound")]
    RequestExceedsHost,
}

impl U3aLimits {
    pub fn try_new(values: U3aLimitValues) -> Result<Self, U3aLimitsError> {
        let positive =
            |field, value| NonZeroUsize::new(value).ok_or(U3aLimitsError::Zero { field });
        let limits = Self {
            request_dto_bytes: positive("request_dto_bytes", values.request_dto_bytes)?,
            nested_items: positive("nested_items", values.nested_items)?,
            page_bytes: positive("page_bytes", values.page_bytes)?,
        };
        if limits.request_dto_bytes.get() > MAX_REQUEST_DTO_BYTES {
            return Err(U3aLimitsError::RequestExceedsHost);
        }
        Ok(limits)
    }

    pub fn production() -> Result<Self, U3aLimitsError> {
        Self::try_new(U3A_PRODUCTION_LIMITS)
    }

    pub fn receipt(self) -> U3aLimitValues {
        U3aLimitValues {
            request_dto_bytes: self.request_dto_bytes.get(),
            nested_items: self.nested_items.get(),
            page_bytes: self.page_bytes.get(),
        }
    }
}
