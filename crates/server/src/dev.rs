//! Live reload for scripts/dev.sh (only when DUET_DEV is set). The page holds an EventSource on
//! /api/dev/reload and reloads when the version it hears changes: a new server process, or a
//! newer build in the web directory.

use std::{convert::Infallible, path::PathBuf, time::Duration};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, stream};

fn newest_mtime(dir: &PathBuf) -> u128 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .max()
        .unwrap_or(0)
}

pub fn reload_stream(web_dir: PathBuf, instance: u128) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = stream::unfold(None::<String>, move |last| {
        let web_dir = web_dir.clone();
        async move {
            loop {
                let dir = web_dir.clone();
                let version = format!("{instance}-{}", tokio::task::spawn_blocking(move || newest_mtime(&dir)).await.unwrap_or(0));
                if last.as_deref() != Some(version.as_str()) {
                    return Some((Ok(Event::default().data(version.clone())), Some(version)));
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
