//! Own one managed protection helper during an active profile transaction.
use std::io;

/// A resource scope, never a cached permission decision. Every operation still
/// checks the current object. Close it after startup or credential work, before
/// waiting for ordinary service requests.
#[derive(Debug)]
#[must_use]
pub struct ProtectionBatch {
    #[cfg(windows)]
    active: bool,
}

impl ProtectionBatch {
    pub fn begin() -> io::Result<Self> {
        #[cfg(windows)]
        super::windows::begin_batch()?;
        Ok(Self {
            #[cfg(windows)]
            active: true,
        })
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.close()
    }

    fn close(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if self.active {
            self.active = false;
            super::windows::end_batch()?;
        }
        Ok(())
    }
}

impl Drop for ProtectionBatch {
    fn drop(&mut self) {
        let _closed = self.close();
    }
}
