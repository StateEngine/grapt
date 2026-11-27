//! Authenticated content and subscription actions.
//! Returned server messages have user-content authority.
use super::{
    BoundedMap, BoundedString, BoundedVec, LocationInfoDto, McpResourceCatalogDto,
    PromptInputFileAttachmentDto,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpContentRequestDto {
    Prompt {
        name: BoundedString<128>,
        arguments: BoundedMap<BoundedString<128>, BoundedString<65536>, 32>,
    },
    Resources {},
    PinResource {
        uri: BoundedString<4096>,
        pinned: bool,
    },
    ReadResource {
        uri: BoundedString<4096>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpUserContentDto {
    pub text: BoundedString<65536>,
    pub files: BoundedVec<PromptInputFileAttachmentDto, 4>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpContentResponseDto {
    Subscription {
        location: LocationInfoDto,
        notice: BoundedString<1024>,
    },
    UserContent {
        location: LocationInfoDto,
        content: McpUserContentDto,
    },
    Resources {
        location: LocationInfoDto,
        catalog: McpResourceCatalogDto,
    },
}

/// Live only: resource content is fetched solely by an explicit read command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpResourceWatchDto {
    pub server: BoundedString<1024>,
    pub uri: BoundedString<4096>,
    pub updated: bool,
}
