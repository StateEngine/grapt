// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified by Grapt: public DTO pages, JSON, bounded output and no-overwrite publication.
use super::Completion;
use grapt_client::{wire::Spelled, Client, Connection};
use std::path::PathBuf;

mod file;
mod markdown;

#[derive(Clone, Copy)]
enum Format {
    Markdown,
    Json,
}
impl Format {
    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

pub(super) fn execute<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    match save(client, args) {
        Ok(path) => {
            done.consume = true;
            done.notice = format!("Session exported to {}", path.display());
        }
        Err(error) => done.notice = format!("Export refused: {error}"),
    }
}

fn save<C: Connection>(client: &mut Client<C>, args: &str) -> Result<PathBuf, String> {
    let args = args.trim();
    let (format, path) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let format = match format {
        "" | "markdown" => Format::Markdown,
        "json" => Format::Json,
        _ => return Err("Use /export [markdown|json] [path].".into()),
    };
    let session = client
        .selected()
        .ok_or_else(|| "Select a session first.".to_owned())?;
    let path = path.trim();
    // The rest of the line is one path, including spaces; no shell expansion.
    let path = if path.starts_with('"') || path.starts_with('\'') {
        let quote = path.chars().next().ok_or("Missing path.")?;
        path.strip_prefix(quote)
            .and_then(|p| p.strip_suffix(quote))
            .filter(|p| !p.is_empty())
            .ok_or("Unclosed or empty quoted path.")?
    } else {
        path
    };
    let path = if path.is_empty() {
        PathBuf::from(format!(
            "{}-export.{}",
            session.spelled(),
            format.extension()
        ))
    } else {
        PathBuf::from(path)
    };
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(format!(
                "{} already exists; choose a new path.",
                path.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let history = client.export_history().map_err(|error| error.to_string())?;
    file::publish(&history, format, &path)?;
    Ok(path)
}
