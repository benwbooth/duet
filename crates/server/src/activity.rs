//! What duet is doing in the background, for the jobs pane: pictures being planned and drawn,
//! the full cast being directed and performed ahead of the listener, and the queued jobs
//! (conversions, syncing a book with its recording). Each piece of work reports how far along
//! it is under a key of its own, and clears it when it's done.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime};

use serde_json::json;

use crate::{State, db};

#[derive(Clone)]
struct Task {
    kind: &'static str,
    title: String,
    detail: String,
    done: u32,
    total: u32,
    since: Instant,
    started: SystemTime,
    updated: Instant,
}

static TASKS: LazyLock<Mutex<BTreeMap<String, Task>>> = LazyLock::new(Default::default);

/// Work under way (or how far it's got): `total` 0 when there's no telling.
pub fn set(key: &str, kind: &'static str, title: &str, detail: impl Into<String>, done: u32, total: u32) {
    let mut t = TASKS.lock().unwrap();
    let detail = detail.into();
    match t.get_mut(key) {
        Some(task) => {
            task.title = title.to_string();
            task.detail = detail;
            task.done = done;
            task.total = total;
            task.updated = Instant::now();
        }
        None => {
            t.insert(
                key.to_string(),
                Task { kind, title: title.to_string(), detail, done, total, since: Instant::now(), started: SystemTime::now(), updated: Instant::now() },
            );
        }
    }
}

pub fn clear(key: &str) {
    TASKS.lock().unwrap().remove(key);
}

/// Clears it after a while, unless it's been updated since (work that comes in bursts, like
/// lining up the audio as you listen, stays shown between them).
pub fn clear_later(key: &str, after: std::time::Duration) {
    let key = key.to_string();
    let stamp = TASKS.lock().unwrap().get(&key).map(|t| t.updated);
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let mut t = TASKS.lock().unwrap();
        if t.get(&key).map(|t| t.updated) == stamp {
            t.remove(&key);
        }
    });
}

/// A book's title, for naming its work.
pub async fn title(state: &State, asset: i64) -> String {
    match db::asset(&state.db, asset).await {
        Ok(Some(a)) => db::work_detail(&state.db, a.work_id).await.ok().flatten().map(|w| w.title).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Everything going on, newest first: the background work and the jobs queued or running.
pub async fn list(state: &State) -> serde_json::Value {
    let mut out: Vec<serde_json::Value> = TASKS
        .lock()
        .unwrap()
        .iter()
        .map(|(key, t)| {
            json!({
                "key": key,
                "kind": t.kind,
                "title": t.title,
                "detail": t.detail,
                "done": t.done,
                "total": t.total,
                "fraction": if t.total > 0 { (t.done as f64 / t.total as f64).min(1.0) } else { -1.0 },
                "seconds": t.since.elapsed().as_secs(),
                "started": t.started.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            })
        })
        .collect();
    if let Ok(jobs) = crate::jobs::list(state).await {
        for j in jobs.into_iter().filter(|j| j.status == "running" || j.status == "queued") {
            let title = db::work_detail(&state.db, j.work_id).await.ok().flatten().map(|w| w.title).unwrap_or_default();
            out.push(json!({
                "key": format!("job:{}", j.id),
                "kind": j.kind.label(),
                "title": title,
                "detail": if j.status == "queued" { format!("{} (waiting)", j.message) } else { j.message.clone() },
                "done": 0,
                "total": 0,
                "fraction": if j.status == "running" { j.progress.clamp(0.0, 1.0) } else { -1.0 },
                "seconds": 0,
                "started": j.created_at.timestamp(),
            }));
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(v["started"].as_u64().unwrap_or(0)));
    json!(out)
}
