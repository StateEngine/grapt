use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{PluginDeclaration, PluginError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PluginGenerationId(u64);

impl PluginGenerationId {
    pub fn new(value: u64) -> Option<Self> {
        (value > 0).then_some(Self(value))
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub callback: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCallback {
    pub domain: String,
    pub name: String,
    pub callback: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRegistration {
    pub plugin_id: String,
    #[serde(default)]
    pub tools: Vec<PluginTool>,
    #[serde(default)]
    pub callbacks: Vec<PluginCallback>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolRoute {
    pub plugin_id: String,
    pub callback: u64,
    pub generation: PluginGenerationId,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivePlugin {
    pub id: String,
    pub target: String,
    pub origin: crate::PluginOrigin,
    pub source: String,
    pub generation: PluginGenerationId,
    pub status: &'static str,
}

#[derive(Debug)]
pub struct PluginGeneration {
    pub(crate) id: PluginGenerationId,
    pub(crate) declarations: Vec<PluginDeclaration>,
    pub(crate) registrations: Vec<PluginRegistration>,
}

impl PluginGeneration {
    pub(crate) fn validate(
        id: PluginGenerationId,
        declarations: Vec<PluginDeclaration>,
        registrations: Vec<PluginRegistration>,
        registration_limit: usize,
        schema_limit: usize,
    ) -> Result<Self, PluginError> {
        if declarations.len() != registrations.len() {
            return Err(PluginError::Registration(
                "bridge did not return one registration per declaration".to_owned(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut callbacks = BTreeSet::new();
        let mut count = 0usize;
        let mut schema_bytes = 0usize;
        for registration in &registrations {
            if registration.plugin_id.is_empty() || !ids.insert(registration.plugin_id.clone()) {
                return Err(PluginError::Registration(
                    "plugin ids must be non-empty and unique within a generation".to_owned(),
                ));
            }
            for tool in &registration.tools {
                count = count.saturating_add(1);
                schema_bytes = schema_bytes.saturating_add(
                    serde_json::to_vec(&tool.input_schema)
                        .map_err(|error| PluginError::Registration(error.to_string()))?
                        .len(),
                );
                if tool.name.is_empty()
                    || tool.name.len() > 256
                    || !names.insert(tool.name.clone())
                    || tool.callback == 0
                {
                    return Err(PluginError::Registration(
                        "tool names/callbacks must be bounded, non-empty, and unique".to_owned(),
                    ));
                }
            }
            for callback in &registration.callbacks {
                count = count.saturating_add(1);
                if callback.domain.is_empty()
                    || callback.domain.len() > 64
                    || callback.name.is_empty()
                    || callback.name.len() > 128
                    || callback.callback == 0
                    || !callbacks.insert((
                        registration.plugin_id.clone(),
                        callback.domain.clone(),
                        callback.name.clone(),
                        callback.callback,
                    ))
                {
                    return Err(PluginError::Registration(
                        "plugin callbacks must have bounded names and unique non-zero identities"
                            .to_owned(),
                    ));
                }
            }
        }
        if count > registration_limit || schema_bytes > schema_limit {
            return Err(PluginError::Registration(
                "plugin registration catalog exceeds its declared bound".to_owned(),
            ));
        }
        Ok(Self {
            id,
            declarations,
            registrations,
        })
    }

    pub const fn id(&self) -> PluginGenerationId {
        self.id
    }

    pub fn registrations(&self) -> &[PluginRegistration] {
        &self.registrations
    }

    pub fn active(&self) -> Vec<ActivePlugin> {
        self.declarations
            .iter()
            .zip(&self.registrations)
            .map(|(declaration, registration)| ActivePlugin {
                id: registration.plugin_id.clone(),
                target: declaration.target.clone(),
                origin: declaration.origin,
                source: declaration.source.clone(),
                generation: self.id,
                status: "active",
            })
            .collect()
    }
}
