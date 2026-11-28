//! Fixed, read-only acquisition for an explicitly created worktree Session.
//! The source stays selected until all bounded replies have validated.
use super::*;
use crate::wire::{FormLocation, ModelRow, SessionAttachment, SessionRow};

#[derive(Debug, Clone, Copy)]
enum Phase {
    Project,
    Agents,
    Models,
    Default,
    Permissions,
    History,
    Subagents,
    Forms,
    Preset,
    LocationForms,
}
impl Phase {
    fn operation(self) -> Operation {
        match self {
            Self::Project => Operation::ProjectCurrent,
            Self::Agents => Operation::AgentList,
            Self::Models => Operation::ModelList,
            Self::Default => Operation::ModelDefault,
            Self::Permissions => Operation::PermissionRequestList,
            // The session's coherent snapshot, which also proves the service
            // serves snapshots before the session goes live.
            Self::History => Operation::SessionSnapshot,
            Self::Subagents => Operation::SessionSubagentModelGet,
            Self::Forms => Operation::SessionFormList,
            Self::Preset => Operation::SessionPresetGet,
            Self::LocationForms => Operation::FormRequestList,
        }
    }
    fn next(self) -> Option<Self> {
        Some(match self {
            Self::Project => Self::Agents,
            Self::Agents => Self::Models,
            Self::Models => Self::Default,
            Self::Default => Self::Permissions,
            Self::Permissions => Self::History,
            Self::History => Self::Subagents,
            Self::Subagents => Self::Forms,
            Self::Forms => Self::Preset,
            Self::Preset => Self::LocationForms,
            Self::LocationForms => return None,
        })
    }
}

#[derive(Debug)]
pub(super) struct Attachment {
    saved: SessionAttachment,
    row: SessionRow,
    directory: String,
    phase: Phase,
    waiting: bool,
    agents: Vec<crate::wire::AgentRow>,
    models: Vec<ModelRow>,
    default: Option<ModelRow>,
    permissions: Vec<u8>,
    permission_revision: Option<u64>,
    history: Vec<u8>,
    /// The permission revision when the snapshot was read: a later accepted
    /// refresh is newer than the snapshot's asks.
    history_revision: Option<u64>,
    subagents: Option<ModelRow>,
    forms: Vec<crate::wire::FormRow>,
    preset: Option<crate::wire::PresetSnapshot>,
    location_forms: Vec<crate::wire::FormRow>,
}
impl Attachment {
    pub(super) fn new(bytes: &[u8]) -> Result<Self, ClientError> {
        // SessionCreate uses the same canonical saved-row response as SessionGet.
        // In particular, do not fill absent choices from the source catalogs.
        let saved = crate::wire::decode_session_attachment(bytes)?;
        let directory = saved.project.worktree.clone();
        if !creation::absolute_backend_directory(&directory) {
            return Err(ClientError::InvalidInput(
                "Created Session has no absolute backend directory.",
            ));
        }
        Ok(Self {
            saved,
            row: crate::wire::decode_session(bytes)?,
            directory,
            phase: Phase::Project,
            waiting: false,
            agents: Vec::new(),
            models: Vec::new(),
            default: None,
            permissions: Vec::new(),
            permission_revision: None,
            history: Vec::new(),
            history_revision: None,
            subagents: None,
            forms: Vec::new(),
            preset: None,
            location_forms: Vec::new(),
        })
    }

    fn request(&self) -> Result<WireRequest, ClientError> {
        let operation = self.phase.operation();
        let mut request = WireRequest::new(operation, Some(&self.saved.id), None)?;
        let directory = match self.phase {
            Phase::Project | Phase::LocationForms => Some(&self.directory),
            Phase::Agents | Phase::Models | Phase::Default => Some(&self.saved.project.worktree),
            _ => None,
        };
        if let Some(directory) = directory {
            request = crate::wire::with_worktree_location(request, directory, Some(&self.saved.id));
        }
        Ok(request)
    }

    fn check_catalog_location(
        &self,
        location: &grapt_api::u3a::LocationInfoDto,
    ) -> Result<(), ClientError> {
        if location.project().get().id().get() != &self.saved.project.id
            || location.project().get().directory().get().as_str() != self.saved.project.worktree
            || ![
                self.directory.as_str(),
                self.saved.project.worktree.as_str(),
            ]
            .contains(&location.directory().get().as_str())
        {
            return Err(ClientError::InvalidInput(
                "Attachment catalog belongs to another checkout.",
            ));
        }
        Ok(())
    }

    fn accept(&mut self, bytes: Vec<u8>, bounds: Bounds) -> Result<bool, ClientError> {
        use grapt_api::u3a::{
            decode_page, AgentListResponseDto, ModelDefaultResponseDto, ModelListResponseDto,
        };
        match self.phase {
            Phase::Project => {
                let (id, root) = crate::wire::decode_project_current(&bytes)?;
                // ProjectCurrent returns only id + checkout root, not a cwd
                // echo. Validate the actual root against the saved directory;
                // equal ProjectIds alone cannot distinguish linked worktrees.
                if id != self.saved.project.id || !contains_saved_directory(&root, &self.directory)
                {
                    return Err(ClientError::InvalidInput(
                        "Created Session resolved to another project or checkout root.",
                    ));
                }
                self.saved.project.worktree = root;
            }
            Phase::Agents => {
                let dto: AgentListResponseDto = decode_page(&bytes, crate::wire::limits()?)
                    .map_err(crate::wire::WireError::from)?;
                self.check_catalog_location(dto.location().get())?;
                self.agents = crate::wire::decode_agents(&bytes)?;
            }
            Phase::Models => {
                let dto: ModelListResponseDto = decode_page(&bytes, crate::wire::limits()?)
                    .map_err(crate::wire::WireError::from)?;
                self.check_catalog_location(dto.location().get())?;
                self.models = crate::wire::decode_models(&bytes)?;
            }
            Phase::Default => {
                let dto: ModelDefaultResponseDto = decode_page(&bytes, crate::wire::limits()?)
                    .map_err(crate::wire::WireError::from)?;
                self.check_catalog_location(dto.location().get())?;
                self.default = crate::wire::decode_model_default(&bytes)?;
            }
            Phase::Permissions => {
                crate::wire::decode_permissions(&bytes)?;
                self.permissions = bytes;
            }
            Phase::History => {
                let snapshot = super::snapshot::decode(&bytes)?;
                history::tail_window(&self.saved.id, &snapshot.page, bounds)?;
                self.history = bytes;
            }
            // Subagent/preset DTOs have no session identity member; requests are
            // addressed only through the canonical created SessionWireId.
            Phase::Subagents => self.subagents = crate::wire::decode_subagent_model(&bytes)?,
            Phase::Forms => {
                let rows = crate::wire::decode_session_forms(&bytes)?;
                if rows.iter().any(|row| {
                    !matches!(
                        &row.owner, crate::wire::FormOwner::Session(id) if id == &self.saved.id
                    )
                }) {
                    return Err(ClientError::InvalidInput(
                        "Attachment form belongs to another Session.",
                    ));
                }
                self.forms = rows;
            }
            Phase::Preset => self.preset = Some(crate::wire::decode_preset(&bytes)?),
            Phase::LocationForms => {
                let (owner, rows) = crate::wire::decode_location_forms(&bytes)?;
                if owner != self.location() {
                    return Err(ClientError::InvalidInput(
                        "Created Session forms belong to another location.",
                    ));
                }
                self.location_forms = rows;
            }
        }
        self.waiting = false;
        if let Some(next) = self.phase.next() {
            self.phase = next;
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn location(&self) -> FormLocation {
        FormLocation {
            project: self.saved.project.id.clone(),
            directory: self.directory.clone(),
        }
    }
}

fn contains_saved_directory(root: &str, directory: &str) -> bool {
    if !creation::absolute_backend_directory(root)
        || root.len() > 8192
        || root.chars().any(char::is_control)
    {
        return false;
    }
    // This is wire-spelling validation, never host filesystem resolution.
    let windows = root.starts_with("\\\\") || root.as_bytes().get(1) == Some(&b':');
    let (root, directory) = if windows {
        (root.replace('\\', "/"), directory.replace('\\', "/"))
    } else {
        (root.to_owned(), directory.to_owned())
    };
    let boundary = root.trim_end_matches('/');
    if boundary.is_empty() {
        return !windows && root == "/" && directory.starts_with('/');
    }
    directory
        .strip_prefix(boundary)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

impl<C: Connection> Client<C> {
    /// Whether a worktree attachment's operator read is in flight.
    pub(super) fn worktree_attachment_waiting(&self) -> bool {
        self.worktree
            .attachment
            .as_ref()
            .is_some_and(|attachment| attachment.waiting)
    }

    /// Exactly one start/poll per call; the old Session's recovery lane stays free.
    pub(super) fn advance_worktree_attachment(
        &mut self,
    ) -> Result<Option<Vec<String>>, ClientError> {
        let mut attachment = self
            .worktree
            .attachment
            .take()
            .ok_or(ClientError::NoSelection)?;
        let operation = attachment.phase.operation();
        if !attachment.waiting {
            match attachment.phase {
                Phase::Permissions => {
                    attachment.permission_revision = Some(self.permission_revision)
                }
                Phase::History => attachment.history_revision = Some(self.permission_revision),
                _ => {}
            }
        }
        let response = if attachment.waiting {
            self.connection.poll_operator()?
        } else {
            // Host-started I/O never coexists with a pending resume.
            self.abandon_resume();
            self.connection.begin_operator(&attachment.request()?)?
        };
        let Some(response) = response else {
            attachment.waiting = true;
            self.worktree.attachment = Some(attachment);
            return Ok(None);
        };
        let bytes = self.accept_response(operation, response)?;
        if attachment.accept(bytes, self.bounds)? {
            match self.install_worktree_attachment(attachment) {
                Ok(lines) => Ok(Some(lines)),
                Err(error) => {
                    // Never leave a partly installed target ready for prompts.
                    self.model = None;
                    Err(error)
                }
            }
        } else {
            self.worktree.attachment = Some(attachment);
            Ok(None)
        }
    }

    /// No Connection::call or request starts: install already validated replies
    /// through the normal selection, history, catalog, form and preset owners.
    fn install_worktree_attachment(
        &mut self,
        mut attachment: Attachment,
    ) -> Result<Vec<String>, ClientError> {
        let id = attachment.saved.id.clone();
        let location = attachment.location();
        let preset = attachment.preset.take().ok_or(ClientError::InvalidInput(
            "Attachment preset was not acquired.",
        ))?;
        // Global asks remain interactive while later attachment reads wait.
        // A newer accepted refresh wins over this earlier staged list, and over
        // the snapshot's asks when it was accepted after the snapshot's read.
        let snapshot_permissions = attachment.history_revision == Some(self.permission_revision);
        if attachment.permission_revision == Some(self.permission_revision) {
            self.accept_permissions(attachment.permissions)?;
        }
        self.prepare_session_selection(&id)?;
        self.begin_tail();
        self.install_attachment_catalog(attachment.agents, attachment.models, attachment.default);
        let project = attachment.saved.project;
        self.project = Some(project.clone());
        self.catalog_refresh.binding = Some(catalog_refresh::Binding {
            session: id.clone(),
            project,
            directory: attachment.directory,
        });
        self.launch_project = None;
        self.agent = attachment
            .saved
            .agent
            .filter(|id| self.saved_agent_offered(id));
        self.model = attachment.saved.model.and_then(|selected| {
            self.models
                .iter()
                .find(|row| {
                    row.id == selected.id
                        && row.provider_id == selected.provider_id
                        && row.variant == selected.variant
                        && row.enabled
                })
                .cloned()
        });
        self.subagent_model = Some(attachment.subagents);
        // Installed at the snapshot's committed position, never at zero. Its
        // forms are older than the form list read after it, which installs
        // next, and its asks only count when no refresh came after it.
        self.install_snapshot_sections(
            &id,
            &attachment.history,
            super::snapshot::Sections {
                permissions: snapshot_permissions,
                forms: false,
            },
        )?;
        self.forms = attachment
            .forms
            .into_iter()
            .take(self.bounds.max_retained_permissions)
            .collect();
        self.session_forms_dirty = false;
        self.install_location_form_rows(location, attachment.location_forms);
        self.location_forms_dirty = false;
        self.mcp_servers.clear();
        self.mcp_refusal = None;
        self.mcp_login = None;
        self.mcp_approval = None;
        self.status_account = Ok(None);
        self.workspace_diff = Ok(None);
        self.preset.snapshot = Some(preset);
        // Explicit launch policy still belongs to the normal preset owner.
        // Without one, the acquired readback replaces its redundant read job.
        if self.preset.launch.is_none() {
            self.preset.job = None;
        }
        self.request_queued_inputs(&id);
        self.draft_creation = creation::Draft::Inactive;
        self.sessions.retain(|row| row.id != id);
        self.sessions.insert(
            0,
            SessionSummary {
                id: attachment.row.id,
                title: attachment.row.title,
                parent: attachment.row.parent,
                detail: attachment.row.detail,
            },
        );
        self.sessions.truncate(self.bounds.max_retained_sessions);
        Ok(crate::wire::worktree_session_selected(&id))
    }
}
