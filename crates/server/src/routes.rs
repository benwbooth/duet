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
    ScanReport, SearchResult, Settings, SyncMap, WorkDetail, WorkSummary,
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
    State, ai, book, db, downloads,
    error::{ApiError, ApiResult},
    free, jobs, library, torznab,
};

pub fn router(state: State, web_dir: &FsPath) -> Router {
    let api = Router::new()
        .route("/library", get(list_works))
        .route("/library/scan", post(scan))
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
        .route("/search", get(search))
        .route("/downloads", get(list_downloads).post(new_download))
        .route("/assistant/ask", post(ask))
        .route("/assistant/transcribe", post(transcribe))
        // Positions, sync maps and job status change under the browser; covers and book
        // resources set their own caching.
        .layer(SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache")))
        .with_state(state);
    let index = web_dir.join("index.html");
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
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

async fn sync(AxState(s): AxState<State>, Path(id): Path<i64>) -> ApiResult<Json<Option<SyncMap>>> {
    Ok(Json(jobs::sync_map(&s, id).await?))
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
    Ok(Json(jobs::enqueue(&s, j.kind, j.work_id).await?))
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
    kind: AssetKind,
}

#[derive(serde::Serialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    errors: Vec<String>,
}

async fn search(AxState(s): AxState<State>, Query(q): Query<SearchQuery>) -> ApiResult<Json<SearchResponse>> {
    let settings = s.settings.read().await.clone();
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let indexer_searches = settings
        .indexers
        .iter()
        .filter(|i| i.enabled)
        .map(|ix| async { (ix.name.clone(), torznab::search(&s.http, ix, &q.q, q.kind).await) });
    let (free_results, ix_results) =
        tokio::join!(free::search(&s.http, &settings.free_sources, &q.q, q.kind), futures_util::future::join_all(indexer_searches));
    for r in free_results {
        match r {
            Ok(v) => results.extend(v),
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    for (name, r) in ix_results {
        match r {
            Ok(v) => results.extend(v),
            Err(e) => errors.push(format!("{name}: {e:#}")),
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
