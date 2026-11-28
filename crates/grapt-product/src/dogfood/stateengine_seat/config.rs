//! The seat a user names in their own configuration. Only the user's own
//! documents are read, and one that turns out to belong to a repository is
//! refused: the seat speaks with the owner's authority.
use crate::dogfood::catalog_config::{user_documents, user_owned, CatalogDocument};
use grapt_config::ConfigValue;
use grapt_stateengine_ipc::{AgentId, AgentSelector, ProjectId};
use std::path::{Path, PathBuf};

const KEY: &str = "stateengine_seat";

/// Where the host runs and which external seat this service reports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeatConfig {
    /// The StateEngine state home holding `endpoint.json`.
    pub(super) home: PathBuf,
    /// The StateEngine binary the endpoint pins.
    pub(super) binary: PathBuf,
    pub(super) project: ProjectId,
    pub(super) agent: AgentSelector,
}

/// The seat the service's user configuration names. Without a configured
/// catalog home there is no user configuration, so there is no seat.
pub(super) fn read(service_root: &Path) -> Result<Option<SeatConfig>, String> {
    match user_documents(service_root)? {
        Some(documents) => from_documents(&documents),
        None => Ok(None),
    }
}

/// The last declaration wins; one in a repository's document refuses.
pub(super) fn from_documents(documents: &[CatalogDocument]) -> Result<Option<SeatConfig>, String> {
    let mut selected = None;
    for document in documents {
        let Some(value) = document.value.get(KEY) else {
            continue;
        };
        if !user_owned(document.origin) {
            return Err(document.refusal("project config cannot configure a StateEngine seat"));
        }
        selected = Some(parse(value.value()).map_err(|reason| document.refusal(&reason))?);
    }
    Ok(selected)
}

fn parse(value: &ConfigValue) -> Result<SeatConfig, String> {
    let entries = value
        .as_object()
        .ok_or("stateengine_seat must be an object")?;
    if let Some((unknown, _)) = entries
        .iter()
        .find(|(key, _)| !["home", "binary", "project", "agent"].contains(&key.value().as_str()))
    {
        return Err(format!(
            "stateengine_seat does not take {:?}",
            unknown.value().as_str()
        ));
    }
    let text = |name: &str| {
        value
            .get(name)
            .and_then(|value| value.value().as_str())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("stateengine_seat.{name} must be a nonempty string"))
    };
    let path = |name: &str| {
        let path = PathBuf::from(text(name)?);
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(format!("stateengine_seat.{name} must be an absolute path"))
        }
    };
    Ok(SeatConfig {
        home: path("home")?,
        binary: path("binary")?,
        project: ProjectId(text("project")?.to_owned()),
        agent: match text("agent")? {
            id if id.starts_with("ag_") => AgentSelector::Id(AgentId(id.to_owned())),
            name => AgentSelector::Name(name.to_owned()),
        },
    })
}
