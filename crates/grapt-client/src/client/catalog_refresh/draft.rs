//! Persist explicit catalog choices in the profile-local cache.
use super::*;

impl<C: Connection> Client<C> {
    pub(in crate::client) fn persist_catalog_choice(&mut self) {
        let (Some(root), Some(project)) = (&self.catalog_refresh.root, &self.project) else {
            return;
        };
        let result = (|| -> Result<(), String> {
            if let Some(mut snapshot) = disk::load(root, project, self.bounds.max_response_bytes)? {
                snapshot.choice = Choice::current(self);
                disk::save(root, project, &snapshot, self.bounds.max_response_bytes)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.catalog_refresh.note = Some(error);
        }
    }
}
