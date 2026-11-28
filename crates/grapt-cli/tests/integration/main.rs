//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../bypass_preset.rs"]
mod bypass_preset;
#[path = "../executable_preparation.rs"]
mod executable_preparation;
#[path = "../first_run_command.rs"]
mod first_run_command;
#[path = "../import_commands.rs"]
mod import_commands;
#[path = "../import_files.rs"]
mod import_files;
#[path = "../login_chatgpt.rs"]
mod login_chatgpt;
#[path = "../server_logs.rs"]
mod server_logs;
