//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../admission_coordinates.rs"]
mod admission_coordinates;
#[path = "../clone_inbox.rs"]
mod clone_inbox;
#[path = "../compaction_admission.rs"]
mod compaction_admission;
#[path = "../fold.rs"]
mod fold;
#[path = "../image_admission.rs"]
mod image_admission;
#[path = "../native_mail_wake.rs"]
mod native_mail_wake;
#[path = "../schema_single_commit.rs"]
mod schema_single_commit;
#[path = "../startup_origin.rs"]
mod startup_origin;
#[path = "../step_cap.rs"]
mod step_cap;
#[path = "../sustained_history.rs"]
mod sustained_history;
#[path = "../synthetic_admission.rs"]
mod synthetic_admission;
