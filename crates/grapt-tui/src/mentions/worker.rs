//! One bounded worker; cancellation discards old queries and their results.
use super::token::Token;
use grapt_client::file_finder::{SearchOutcome, SearchRequest};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
};

pub(super) struct Job {
    pub token: Token,
    pub root: PathBuf,
    pub request: SearchRequest,
}
pub(super) struct Reply {
    pub request: SearchRequest,
    pub token: Token,
    pub root: PathBuf,
    pub result: Result<SearchOutcome, String>,
}
pub(super) struct Worker {
    pub jobs: SyncSender<Job>,
    pub replies: Receiver<Reply>,
}
impl Worker {
    pub fn start(wake: SyncSender<()>) -> Result<Self, String> {
        let (jobs, input) = mpsc::sync_channel::<Job>(1);
        let (output, replies) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("tui-r-file-search".into())
            .spawn(move || {
                while let Ok(job) = input.recv() {
                    // Receiving frees bounded dispatch capacity, even for canceled work.
                    let _ = wake.try_send(());
                    let result = job.request.search().map_err(|e| e.to_string());
                    if !job.request.is_current() {
                        continue;
                    }
                    let _ = wake.try_send(());
                    if output
                        .send(Reply {
                            request: job.request,
                            token: job.token,
                            root: job.root,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _ = wake.try_send(());
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { jobs, replies })
    }
}
