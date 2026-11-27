//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../migration_corpus.rs"]
mod migration_corpus;
#[path = "../permission_survival.rs"]
mod permission_survival;
#[path = "../r6_migration.rs"]
mod r6_migration;
