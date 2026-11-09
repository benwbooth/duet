//! SQLite access: works, assets, positions, cast, settings.

use std::{path::Path, str::FromStr};

use anyhow::Context;
use duet_common::{Asset, AssetKind, CastMember, Position, Settings, Track, WorkDetail, WorkSummary};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

pub async fn open(path: &Path) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await.context("migrating database")?;
    Ok(pool)
}

pub async fn load_settings(db: &SqlitePool) -> anyhow::Result<Settings> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM settings WHERE id = 1").fetch_optional(db).await?;
    Ok(match row {
        Some((data,)) => serde_json::from_str(&data).unwrap_or_default(),
        None => Settings::default(),
    })
}

pub async fn save_settings(db: &SqlitePool, s: &Settings) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO settings (id, data) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data")
        .bind(serde_json::to_string(s)?)
        .execute(db)
        .await?;
    Ok(())
}

pub fn cover_url(id: i64, cover: &Option<String>) -> Option<String> {
    // The file name changes with each new cover, so it doubles as a version:
    // the address changes and browsers fetch the new picture.
    cover.as_ref().map(|p| {
        let v = p.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        format!("/api/works/{id}/cover?v={v:x}")
    })
}

pub async fn list_works(db: &SqlitePool) -> anyhow::Result<Vec<WorkSummary>> {
    let rows = sqlx::query(
        "SELECT w.id, w.title, w.author, w.series, w.series_index, w.year, w.cover_path,
            EXISTS(SELECT 1 FROM assets a WHERE a.work_id = w.id AND a.kind = 'ebook') AS has_ebook,
            EXISTS(SELECT 1 FROM assets a WHERE a.work_id = w.id AND a.kind = 'audiobook') AS has_audiobook,
            EXISTS(SELECT 1 FROM sync_maps s WHERE s.work_id = w.id) AS has_sync,
            (SELECT data FROM positions p WHERE p.work_id = w.id) AS position
         FROM works w
         ORDER BY COALESCE((SELECT updated_at FROM positions p WHERE p.work_id = w.id), '') DESC,
                  w.author, w.series, w.series_index, w.title",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let id: i64 = r.get("id");
            let cover: Option<String> = r.get("cover_path");
            let position: Option<String> = r.get("position");
            WorkSummary {
                id,
                title: r.get("title"),
                author: r.get("author"),
                series: r.get("series"),
                series_index: r.get("series_index"),
                year: r.get("year"),
                has_ebook: r.get("has_ebook"),
                has_audiobook: r.get("has_audiobook"),
                has_sync: r.get("has_sync"),
                cover_url: cover_url(id, &cover),
                progress: position.and_then(|p| serde_json::from_str::<Position>(&p).ok()).map(|p| p.fraction),
            }
        })
        .collect())
}

pub async fn work_detail(db: &SqlitePool, id: i64) -> anyhow::Result<Option<WorkDetail>> {
    let Some(r) = sqlx::query("SELECT * FROM works WHERE id = ?").bind(id).fetch_optional(db).await? else {
        return Ok(None);
    };
    let cover: Option<String> = r.get("cover_path");
    let has_sync: (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM sync_maps WHERE work_id = ?)").bind(id).fetch_one(db).await?;
    Ok(Some(WorkDetail {
        id,
        title: r.get("title"),
        author: r.get("author"),
        series: r.get("series"),
        series_index: r.get("series_index"),
        description: r.get("description"),
        year: r.get("year"),
        subjects: r
            .get::<Option<String>, _>("subjects")
            .map(|s| s.split('\n').filter(|x| !x.is_empty()).map(str::to_string).collect())
            .unwrap_or_default(),
        pages: r.get("pages"),
        cover_url: cover_url(id, &cover),
        assets: work_assets(db, id).await?,
        position: get_position(db, id).await?,
        has_sync: has_sync.0,
        cast: cast(db, id).await?,
    }))
}

pub async fn work_assets(db: &SqlitePool, work_id: i64) -> anyhow::Result<Vec<Asset>> {
    let rows = sqlx::query("SELECT * FROM assets WHERE work_id = ? ORDER BY kind, added_at").bind(work_id).fetch_all(db).await?;
    let mut out = Vec::new();
    for r in rows {
        out.push(asset_from_row(db, &r).await?);
    }
    Ok(out)
}

pub async fn asset(db: &SqlitePool, id: i64) -> anyhow::Result<Option<Asset>> {
    match sqlx::query("SELECT * FROM assets WHERE id = ?").bind(id).fetch_optional(db).await? {
        Some(r) => Ok(Some(asset_from_row(db, &r).await?)),
        None => Ok(None),
    }
}

async fn asset_from_row(db: &SqlitePool, r: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Asset> {
    let id: i64 = r.get("id");
    let kind: String = r.get("kind");
    let tracks = sqlx::query("SELECT idx, title, duration_secs FROM tracks WHERE asset_id = ? ORDER BY idx")
        .bind(id)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|t| {
            let index: i64 = t.get("idx");
            let raw: String = t.get("title");
            Track { index, title: duet_common::text::track_title(&raw, index as usize), duration_secs: t.get("duration_secs"), url: format!("/api/assets/{id}/tracks/{index}") }
        })
        .collect();
    Ok(Asset {
        id,
        work_id: r.get("work_id"),
        kind: AssetKind::parse(&kind).unwrap_or(AssetKind::Ebook),
        format: r.get("format"),
        path: r.get("path"),
        size: r.get("size"),
        duration_secs: r.get("duration_secs"),
        tracks,
        origin: r.get("origin"),
    })
}

pub async fn track_path(db: &SqlitePool, asset_id: i64, idx: i64) -> anyhow::Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT path FROM tracks WHERE asset_id = ? AND idx = ?")
        .bind(asset_id)
        .bind(idx)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

pub async fn get_position(db: &SqlitePool, work_id: i64) -> anyhow::Result<Option<Position>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT data FROM positions WHERE work_id = ?").bind(work_id).fetch_optional(db).await?;
    Ok(row.and_then(|r| serde_json::from_str(&r.0).ok()))
}

pub async fn set_position(db: &SqlitePool, work_id: i64, mut p: Position) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    p.updated_at = Some(now);
    sqlx::query(
        "INSERT INTO positions (work_id, data, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(work_id) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
    )
    .bind(work_id)
    .bind(serde_json::to_string(&p)?)
    .bind(now.to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn cast(db: &SqlitePool, work_id: i64) -> anyhow::Result<Vec<CastMember>> {
    Ok(sqlx::query("SELECT * FROM cast_members WHERE work_id = ? ORDER BY is_narrator DESC, name")
        .bind(work_id)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|r| CastMember {
            id: r.get("id"),
            name: r.get("name"),
            description: r.get("description"),
            voice: r.get("voice"),
            is_narrator: r.get("is_narrator"),
        })
        .collect())
}

pub async fn upsert_cast(db: &SqlitePool, work_id: i64, m: &CastMember) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO cast_members (work_id, name, description, voice, is_narrator) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(work_id, name) DO UPDATE SET description = excluded.description,
            voice = COALESCE(excluded.voice, cast_members.voice), is_narrator = excluded.is_narrator",
    )
    .bind(work_id)
    .bind(&m.name)
    .bind(&m.description)
    .bind(&m.voice)
    .bind(m.is_narrator)
    .execute(db)
    .await?;
    Ok(())
}
