//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../form_service.rs"]
mod form_service;
#[path = "../global_forms.rs"]
mod global_forms;
#[path = "../schema_single_commit.rs"]
mod schema_single_commit;
