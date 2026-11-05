//! The JSON API and static file serving.

use std::path::{Path as FsPath, PathBuf};

use axum::{
    Json, Router,
    body::Body,
    extract::{Multipart, Path, Query, Request, State as AxState},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use duet_common::{
    AskReply, AskRequest, AssetKind, BookLayout, CastMember, Chapter, Download, Job, NewDownload, NewJob, Position,
    ScanReport, SearchResult, Settings, WorkDetail, WorkSummary,
};
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::{
    State, ai, ai_setup, book, db, downloads,
    error::{ApiError, ApiResult},
    free, jobs, library, setup, torznab,
};

pub fn router(state: State, web_dir: &FsPath) -> Router {
    let api = Router::new()
        .route("/library", get(list_works))
        .route("/library/scan", post(scan))
        .route("/folders", get(folders))
        .route("/works/{id}", get(work).patch(edit_work))
        .route("/works/{id}/cover", get(cover))
        .route("/works/{id}/position", get(get_position).put(put_position))
        .route("/works/{id}/sync", get(sync))
        .route("/works/{id}/cast", post(save_cast))
        .route("/assets/{id}/file", get(asset_file))
        .route("/assets/{id}/layout", get(layout))
        .route("/assets/{id}/chapters/{n}", get(chapter))
        .route("/assets/{id}/res/{*path}", get(resource))
        .route("/assets/{id}/locate", get(locate))
        .route("/assets/{id}/tracks/{n}", get(track))
        .route("/assets/{id}/link", post(link))
        .route("/jobs", get(list_jobs).post(new_job))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/settings/test-qbittorrent", post(test_qbit))
        .route("/setup/voices", get(voices_status))
        .route("/setup/voices/install", post(voices_install))
        .route("/setup/voices/start", post(voices_start))
        .route("/setup/voices/stop", post(voices_stop))
        .route("/setup/voices/log", get(voices_log))
        .route("/search", get(search))
        .route("/downloads", get(list_downloads).post(new_download))
        .route("/ai/models", post(ai_models))
        .route("/ai/setup", get(ai_detect))
        .route("/ai/pull", post(ai_pull))
        .route("/ai/test", post(ai_test))
        .route("/assistant/ask", post(ask))
        .route("/assistant/transcribe", post(transcribe))
        // Positions, sync maps and job status change under the browser; covers and book
        // resources set their own caching.
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache")))
        .with_state(state);
    let index = web_dir.join("index.html");
    let mut app = Router::new();
    if std::env::var_os("DUET_DEV").is_some() {
        let (dir, instance) = (web_dir.to_path_buf(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        app = app.route("/api/dev/reload", get(move || std::future::ready(crate::dev::reload_stream(dir.clone(), instance))));
    }
    app.nest("/api", api)
        .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
        // Revalidate the UI on every load (a cheap 304) so a new build is picked up at once.
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache")))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

async fn list_works(AxState(s): AxState<State>) -> ApiResult<Json<Vec<WorkSummary>>> {
    Ok(Json(db::list_works(&s.db).await?))
}

async fn scan(AxState(s): AxState<State>) -> ApiResult<Json<ScanReport>> {
    Ok(Json(library::scan(&s).await?))
}

async fn work(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<WorkDetail>> {
    db::work_detail(&s.db, id).await?.map(Json).ok_or_else(|| ApiError::not_found("book"))
}

#[derive(Deserialize)]
struct WorkEdit {
    title: Option<String>,
    author: Option<String>,
    series: Option<String>,
    series_index: Option<f64>,
}

async fn edit_work(AxState(s): AxState<State>, Path(id): Path<i64>, Json(e): Json<WorkEdit>) -> ApiResult<Json<WorkDetail>> {
    sqlx::query(
        "UPDATE works SET title = COALESCE(?, title), author = COALESCE(?, author), series = COALESCE(?, series),
            series_index = COALESCE(?, series_index) WHERE id = ?",
    )
    .bind(e.title)
    .bind(e.author)
    .bind(e.series)
    .bind(e.series_index)
    .bind(id)
    .execute(&s.db)
    .await?;
    work(AxState(s), Path(id)).await
}

async fn serve_path(path: PathBuf, req: Request) -> ApiResult<Response> {
    // ServeFile handles Range requests, which audio seeking needs.
    let resp = ServeFile::new(path).oneshot(req).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(resp.map(Body::new).into_response())
}

async fn cover(AxState(s): AxState<State>, Path(id): Path<i64>, req: Request) -> ApiResult<Response> {
    let (path,): (Option<String>,) =
        sqlx::query_as("SELECT cover_path FROM works WHERE id = ?").bind(id).fetch_optional(&s.db).await?.ok_or_else(|| ApiError::not_found("book"))?;
    let path = path.ok_or_else(|| ApiError::not_found("cover"))?;
    let bytes = tokio::fs::read(&path).await?;
    let _ = req;
    let mime = sniff_image(&bytes);
    Ok(([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "max-age=86400")], bytes).into_response())
}

fn sniff_image(b: &[u8]) -> &'static str {
    match b {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => "image/jpeg",
    }
}

async fn get_position(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<Option<Position>>> {
    Ok(Json(db::get_position(&s.db, id).await?))
}

async fn put_position(AxState(s): AxState<State>, Path(id): Path<i64>, Json(p): Json<Position>) -> ApiResult<StatusCode> {
    db::set_position(&s.db, id, p).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SyncQuery {
    ebook: Option<i64>,
    audio: Option<i64>,
}

/// With ?ebook=&audio=, that pairing's sync map; without, the list of synced pairings.
async fn sync(AxState(s): AxState<State>, Path(id): Path<i64>, Query(q): Query<SyncQuery>) -> ApiResult<Response> {
    Ok(match (q.ebook, q.audio) {
        (Some(e), Some(a)) => Json(jobs::sync_map(&s, e, a).await?).into_response(),
        _ => Json(jobs::synced_pairs(&s, id).await?).into_response(),
    })
}

async fn save_cast(AxState(s): AxState<State>, Path(id): Path<i64>, Json(m): Json<CastMember>) -> ApiResult<Json<Vec<CastMember>>> {
    db::upsert_cast(&s.db, id, &m).await?;
    Ok(Json(db::cast(&s.db, id).await?))
}

async fn asset_or_404(s: &State, id: i64) -> ApiResult<duet_common::Asset> {
    db::asset(&s.db, id).await?.ok_or_else(|| ApiError::not_found("file"))
}

async fn asset_file(AxState(s): AxState<State>, Path(id): Path<i64>, req: Request) -> ApiResult<Response> {
    let a = asset_or_404(&s, id).await?;
    if FsPath::new(&a.path).is_dir() {
        return Err(ApiError::bad_request("this audiobook is a folder; play its tracks"));
    }
    serve_path(PathBuf::from(a.path), req).await
}

async fn layout(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<BookLayout>> {
    let a = asset_or_404(&s, id).await?;
    let src = book::source(&a, &s.data_dir);
    Ok(Json(tokio::task::spawn_blocking(move || book::layout(&src)).await??))
}

async fn chapter(AxState(s): AxState<State>, Path((id, n)): Path<(i64, usize)>) -> ApiResult<Json<Chapter>> {
    let a = asset_or_404(&s, id).await?;
    let src = book::source(&a, &s.data_dir);
    Ok(Json(tokio::task::spawn_blocking(move || book::chapter(&src, id, n)).await??))
}

async fn resource(AxState(s): AxState<State>, Path((id, path)): Path<(i64, String)>) -> ApiResult<Response> {
    let a = asset_or_404(&s, id).await?;
    let src = book::source(&a, &s.data_dir);
    let (bytes, mime) = tokio::task::spawn_blocking(move || book::resource(&src, &path)).await?.map_err(|_| ApiError::not_found("resource"))?;
    Ok(([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "max-age=86400".to_string())], bytes).into_response())
}

#[derive(Deserialize)]
struct LocateQuery {
    path: String,
}

/// Chapter index of a file inside the book, for links between chapters.
async fn locate(AxState(s): AxState<State>, Path(id): Path<i64>, Query(q): Query<LocateQuery>) -> ApiResult<Json<Option<usize>>> {
    let a = asset_or_404(&s, id).await?;
    let src = book::source(&a, &s.data_dir);
    Ok(Json(tokio::task::spawn_blocking(move || book::chapter_of(&src, &q.path)).await?))
}

async fn track(AxState(s): AxState<State>, Path((id, n)): Path<(i64, i64)>, req: Request) -> ApiResult<Response> {
    let path = db::track_path(&s.db, id, n).await?.ok_or_else(|| ApiError::not_found("track"))?;
    serve_path(PathBuf::from(path), req).await
}

#[derive(Deserialize)]
struct LinkReq {
    work_id: i64,
}

async fn link(AxState(s): AxState<State>, Path(id): Path<i64>, Json(r): Json<LinkReq>) -> ApiResult<Json<WorkDetail>> {
    library::link_asset(&s.db, id, r.work_id).await?;
    work(AxState(s), Path(r.work_id)).await
}

async fn list_jobs(AxState(s): AxState<State>) -> ApiResult<Json<Vec<Job>>> {
    Ok(Json(jobs::list(&s).await?))
}

async fn new_job(AxState(s): AxState<State>, Json(j): Json<NewJob>) -> ApiResult<Json<i64>> {
    Ok(Json(jobs::enqueue_pair(&s, j.kind, j.work_id, j.ebook_asset, j.audiobook_asset).await?))
}

async fn get_settings(AxState(s): AxState<State>) -> Json<Settings> {
    Json(s.settings.read().await.clone())
}

async fn put_settings(AxState(s): AxState<State>, Json(new): Json<Settings>) -> ApiResult<Json<Settings>> {
    db::save_settings(&s.db, &new).await?;
    *s.settings.write().await = new.clone();
    Ok(Json(new))
}

async fn test_qbit( Json(q): Json<duet_common::QbitSettings>) -> ApiResult<Json<String>> {
    let q = duet_common::QbitSettings { enabled: true, ..q };
    let fresh = crate::qbit::Qbit::new()?;
    let v = fresh.version(&q).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(format!("Connected to qBittorrent {v}")))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(serde::Serialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    errors: Vec<String>,
}

/// Searches every source for both ebooks and audiobooks at once; the UI groups the hits by book.
async fn search(AxState(s): AxState<State>, Query(q): Query<SearchQuery>) -> ApiResult<Json<SearchResponse>> {
    let settings = s.settings.read().await.clone();
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let kinds = [AssetKind::Ebook, AssetKind::Audiobook];
    let indexer_searches = settings.indexers.iter().filter(|i| i.enabled).flat_map(|ix| {
        let (http, q) = (&s.http, &q.q);
        kinds.map(move |kind| async move { (ix.name.clone(), torznab::search(http, ix, q, kind).await) })
    });
    let (free_ebooks, free_audio, ix_results) = tokio::join!(
        free::search(&s.http, &settings.free_sources, &q.q, AssetKind::Ebook),
        free::search(&s.http, &settings.free_sources, &q.q, AssetKind::Audiobook),
        futures_util::future::join_all(indexer_searches)
    );
    for r in free_ebooks.into_iter().chain(free_audio) {
        match r {
            Ok(v) => results.extend(v),
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    for (name, r) in ix_results {
        match r {
            Ok(v) => results.extend(v),
            Err(e) => {
                let msg = format!("{name}: {e:#}");
                if !errors.contains(&msg) {
                    errors.push(msg);
                }
            }
        }
    }
    Ok(Json(SearchResponse { results, errors }))
}

async fn list_downloads(AxState(s): AxState<State>) -> ApiResult<Json<Vec<Download>>> {
    Ok(Json(downloads::list(&s).await?))
}

async fn new_download(AxState(s): AxState<State>, Json(d): Json<NewDownload>) -> ApiResult<StatusCode> {
    downloads::start(&s, d).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(StatusCode::ACCEPTED)
}

async fn ask(AxState(s): AxState<State>, Json(r): Json<AskRequest>) -> ApiResult<Json<AskReply>> {
    Ok(Json(ai::ask(&s, r).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?))
}

/// Microphone audio from the browser (multipart field "audio") to text.
async fn transcribe(AxState(s): AxState<State>, mut form: Multipart) -> ApiResult<Json<String>> {
    while let Some(field) = form.next_field().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
        if field.name() == Some("audio") {
            let filename = field.file_name().unwrap_or("speech.webm").to_string();
            let mime = field.content_type().unwrap_or("audio/webm").to_string();
            let bytes = field.bytes().await.map_err(|e| ApiError::bad_request(e.to_string()))?;
            let text = ai::transcribe(&s, bytes.to_vec(), filename, mime).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
            return Ok(Json(text));
        }
    }
    Err(ApiError::bad_request("no audio"))
}

async fn voices_status(AxState(s): AxState<State>) -> Json<duet_common::VoiceSetup> {
    Json(setup::status(&s).await)
}

async fn voices_install(AxState(s): AxState<State>) -> ApiResult<Json<duet_common::VoiceSetup>> {
    setup::install(&s).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(setup::status(&s).await))
}

async fn voices_start(AxState(s): AxState<State>) -> ApiResult<Json<duet_common::VoiceSetup>> {
    setup::start_server(&s).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(setup::status(&s).await))
}

async fn voices_stop(AxState(s): AxState<State>) -> Json<duet_common::VoiceSetup> {
    setup::stop_server(&s).await;
    Json(setup::status(&s).await)
}

/// The last lines of the voice server's log, for troubleshooting from the UI.
async fn voices_log(AxState(s): AxState<State>) -> ApiResult<Json<Vec<String>>> {
    let text = tokio::fs::read_to_string(setup::log_path(&s.data_dir)).await.unwrap_or_default();
    let lines: Vec<String> = text.lines().rev().take(80).map(str::to_string).collect();
    Ok(Json(lines.into_iter().rev().collect()))
}

#[derive(Deserialize)]
struct ModelsQuery {
    url: String,
    #[serde(default)]
    key: String,
}

async fn ai_models(AxState(s): AxState<State>, Json(q): Json<ModelsQuery>) -> ApiResult<Json<Vec<String>>> {
    Ok(Json(ai::list_models(&s.http, &q.url, &q.key).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?))
}

async fn ai_detect(AxState(s): AxState<State>) -> Json<duet_common::ChatSetup> {
    Json(ai_setup::detect(&s).await)
}

#[derive(Deserialize)]
struct PullReq {
    url: String,
    model: String,
}

async fn ai_pull(AxState(s): AxState<State>, Json(r): Json<PullReq>) -> ApiResult<Json<duet_common::ChatSetup>> {
    ai_setup::pull(&s, &r.url, &r.model).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(ai_setup::detect(&s).await))
}

async fn ai_test(AxState(s): AxState<State>) -> ApiResult<Json<duet_common::ChatTest>> {
    Ok(Json(ai_setup::test(&s).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?))
}

#[derive(Deserialize)]
struct FolderQuery {
    path: Option<String>,
}

/// Lists sub-folders for the folder picker. Starts in the home folder with suggestions.
async fn folders(Query(q): Query<FolderQuery>) -> ApiResult<Json<duet_common::FolderListing>> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
    let start = q.path.is_none();
    let path = q.path.map(PathBuf::from).unwrap_or_else(|| home.clone());
    tokio::task::spawn_blocking(move || {
        let books_in = |dir: &FsPath| {
            std::fs::read_dir(dir)
                .map(|d| {
                    d.flatten()
                        .take(2000)
                        .filter(|e| {
                            let ext = library::ext_of(&e.path());
                            library::EBOOK_EXTS.contains(&ext.as_str()) || library::AUDIO_EXTS.contains(&ext.as_str())
                        })
                        .count()
                })
                .unwrap_or(0)
        };
        let entry = |p: PathBuf| duet_common::FolderEntry {
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned()),
            books: books_in(&p),
            path: p.to_string_lossy().into_owned(),
        };
        let mut folders: Vec<duet_common::FolderEntry> = std::fs::read_dir(&path)
            .map_err(|e| ApiError::bad_request(format!("can't open {}: {e}", path.display())))?
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| entry(e.path()))
            .collect();
        folders.sort_by_key(|f| f.name.to_lowercase());
        let suggestions = if start {
            ["Books", "Ebooks", "eBooks", "Audiobooks", "Documents/Books", "Music/Audiobooks", "Calibre Library", "Downloads"]
                .iter()
                .map(|d| home.join(d))
                .filter(|p| p.is_dir())
                .map(entry)
                .collect()
        } else {
            Vec::new()
        };
        Ok(Json(duet_common::FolderListing {
            parent: path.parent().map(|p| p.to_string_lossy().into_owned()),
            path: path.to_string_lossy().into_owned(),
            folders,
            suggestions,
        }))
    })
    .await?
}
