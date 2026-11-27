use super::*;
use std::io::{Read, Write};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn start(entry: Arc<Entry>, spec: ExecSpec) -> Result<(), process::SpawnFailure> {
    if entry.cancelled.load(Ordering::Acquire) {
        return Err("terminal startup was cancelled before spawn".into());
    }
    #[cfg(windows)]
    let mut spawned = process::spawn(
        &spec,
        entry
            .history
            .as_ref()
            .and_then(|history| history.observed_ms()),
    )?;
    #[cfg(not(windows))]
    let mut spawned = process::spawn(&spec)?;
    // Both native adapters acknowledge authenticated helper handoff before
    // returning. The helper now owns its pins; transcript retention must not.
    drop(spec);
    #[cfg(any(windows, target_os = "linux"))]
    {
        lock(&entry.observation).enforcement = spawned.enforcement.clone();
    }
    let control = Arc::clone(&spawned.control);
    *lock(&entry.control) = Some(Arc::clone(&control));
    let readers = std::mem::take(&mut spawned.readers);
    lock(&entry.observation).readers = readers.len();
    let (input, mut receiver) = mpsc::channel::<Input>(1);
    *lock(&entry.input) = Some(input);
    let mut writer = std::mem::replace(&mut spawned.writer, Box::new(std::io::sink()));
    let watcher = Arc::clone(&entry);
    tokio::spawn(async move {
        supervise(watcher, control).await;
    });
    spawned.disarm();
    lock(&entry.observation).writer_open = true;
    let writer_entry = Arc::clone(&entry);
    let writer_started = std::thread::Builder::new()
        .name("grapt-terminal-input".into())
        .spawn(move || {
            // Keeping the entry here also keeps its global resource permit while
            // a platform write is blocked, even if the model's wait has expired.
            #[cfg(windows)]
            let mut tty_input = super::windows_input::WindowsTtyInputNormalizer::default();
            while let Some(input) = receiver.blocking_recv() {
                if writer_entry.cancelled.load(Ordering::Acquire) {
                    let _reply = input.reply.send(Err("terminal was cancelled".into()));
                    break;
                }
                #[cfg(windows)]
                let bytes = if writer_entry.spec.tty {
                    tty_input.normalise(&input.bytes)
                } else {
                    input.bytes
                };
                #[cfg(not(windows))]
                let bytes = input.bytes;
                let result = writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .map_err(|error| format!("terminal input write failed: {error}"));
                let failed = result.is_err();
                let _reply = input.reply.send(result);
                if failed {
                    writer_entry.cancel();
                    break;
                }
            }
            drop(writer);
            lock(&writer_entry.observation).writer_open = false;
            writer_entry.changed.notify_waiters();
        });
    if let Err(error) = writer_started {
        // No output worker has started. Close handles before clearing counts.
        drop(readers);
        let mut data = lock(&entry.observation);
        data.writer_open = false;
        data.readers = 0;
        data.pipe = PipeState::Drained;
        drop(data);
        entry.cancel();
        return Err(format!("terminal input worker failed: {error}").into());
    }
    let reader_count = readers.len();
    for (index, mut reader) in readers.into_iter().enumerate() {
        let reader_entry = Arc::clone(&entry);
        let started = std::thread::Builder::new()
            .name("grapt-terminal-output".into())
            .spawn(move || {
                let entry = reader_entry;
                let mut bytes = [0_u8; 8192];
                loop {
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) => {
                            let result = {
                                let mut data = lock(&entry.observation);
                                if data.pipe.is_settled() {
                                    break;
                                }
                                data.output.append(&bytes[..count])
                            };
                            if let Err(error) = result {
                                lock(&entry.observation).cleanup_error = Some(error.into());
                                entry.cancel();
                                break;
                            }
                            entry.changed.notify_waiters();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        // Linux PTYs report EIO after the last slave descriptor
                        // closes. Ordinary pipes never treat EIO as EOF.
                        #[cfg(target_os = "linux")]
                        Err(error)
                            if entry.spec.tty && error.raw_os_error() == Some(nix::libc::EIO) =>
                        {
                            break;
                        }
                        Err(error) => {
                            let mut data = lock(&entry.observation);
                            data.pipe = PipeState::TimedOut;
                            data.cleanup_error =
                                Some(format!("terminal output read failed: {error}"));
                            break;
                        }
                    }
                }
                drop(reader);
                let mut data = lock(&entry.observation);
                data.readers = data.readers.saturating_sub(1);
                if data.readers == 0 && data.pipe == PipeState::Open {
                    data.pipe = PipeState::Drained;
                }
                drop(data);
                entry.changed.notify_waiters();
            });
        if let Err(error) = started {
            let mut data = lock(&entry.observation);
            data.readers = data.readers.saturating_sub(reader_count - index);
            data.pipe = PipeState::TimedOut;
            drop(data);
            entry.cancel();
            return Err(format!("terminal output worker failed: {error}").into());
        }
    }
    // A queued blocking spawn can outlive its caller or session. If cancellation
    // raced with OS startup, clear the newly installed input and request cleanup
    // before returning an uncertain effect to the existing runtime owner.
    if entry.cancelled.load(Ordering::Acquire) {
        entry.cancel();
        return Err("terminal startup was cancelled after process creation".into());
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn supervise(entry: Arc<Entry>, control: Arc<Mutex<process::Control>>) {
    let lifetime = tokio::time::sleep(Duration::from_millis(entry.spec.timeout_ms));
    tokio::pin!(lifetime);
    let mut requested = None;
    let mut failure = None;
    let observed = loop {
        if entry.cancelled.load(Ordering::Acquire) {
            requested = Some(ShellTerminal::Cancelled);
            break None;
        }
        match lock(&control).poll() {
            Ok(Some(exit)) => break Some(exit),
            Ok(None) => (),
            Err(error) => {
                failure = Some(error);
                break None;
            }
        }
        tokio::select! {
            () = &mut lifetime => { requested = Some(ShellTerminal::TimedOut); break None; }
            () = entry.cancel.notified() => (),
            () = tokio::time::sleep(Duration::from_millis(50)) => (),
        }
    };
    let observed = match observed {
        Some(exit) => Some(exit),
        None => {
            if let Err(error) = lock(&control).cancel() {
                failure = Some(error);
            }
            let reaped = tokio::time::timeout(STOP_SETTLE_BOUND, async {
                loop {
                    match lock(&control).poll() {
                        Ok(Some(exit)) => return Ok(exit),
                        Ok(None) => (),
                        Err(error) => return Err(error),
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await;
            match reaped {
                Ok(Ok(exit)) => Some(exit),
                Ok(Err(error)) => {
                    failure = Some(error);
                    None
                }
                Err(_) => {
                    failure = Some(format!(
                        "terminal cleanup did not confirm process exit within {} ms",
                        STOP_SETTLE_BOUND.as_millis()
                    ));
                    None
                }
            }
        }
    };
    lock(&entry.input).take();
    #[cfg(windows)]
    let msys_note = {
        let observed_msys = lock(&control).finish_msys_observation();
        entry
            .spec
            .environment
            .sandbox()
            .filter(|plan| !plan.git_metadata_read_only())
            .and(observed_msys)
            .and_then(|observed_msys| {
                observed
                    .as_ref()
                    .and_then(|(code, _)| sandbox::msys::failure_note(*code, observed_msys))
            })
    };
    {
        let mut data = lock(&entry.observation);
        #[cfg(windows)]
        if let Some(note) = msys_note {
            data.process_notes.push(note);
        }
        data.exit = Some(match (requested, observed.as_ref()) {
            (Some(exit), _) => exit,
            (None, Some((code, _))) => ShellTerminal::Exited(*code),
            (None, None) => ShellTerminal::Failed(
                failure
                    .clone()
                    .unwrap_or_else(|| "terminal exit could not be observed".into()),
            ),
        });
        if let Some((_, Some(error))) = observed {
            failure = Some(error);
        }
        if let Some(error) = failure.as_ref() {
            data.cleanup_error = Some(error.clone());
        }
    }
    entry.changed.notify_waiters();
    let drained = tokio::time::timeout(DRAIN_GRACE, async {
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let drained = {
                let data = lock(&entry.observation);
                data.readers == 0 && !data.writer_open
            };
            if drained {
                break;
            }
            changed.await;
        }
    })
    .await;
    {
        let mut data = lock(&entry.observation);
        if drained.is_err() && data.pipe == PipeState::Open {
            data.pipe = PipeState::TimedOut;
        }
        if failure.is_none() {
            data.cleanup = CleanupState::Complete;
        }
        data.supervised = true;
    }
    entry.persist_end();
    entry.changed.notify_waiters();
}
