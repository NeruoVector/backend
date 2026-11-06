//! Product photo originals live on the server's persistent local volume.
use axum::{Json, Router, extract::{Path, State}, http::header, middleware, response::{IntoResponse, Response}, routing::{get, post}};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{io::ErrorKind, path::PathBuf};
use tokio::sync::Mutex;
use uuid::Uuid;
use crate::{auth, error::AppError, state::AppState};

const MAX_BYTES: usize = 1024 * 1024;
// Serialize disk publication and old-version cleanup within this server process.
static UPLOAD: Mutex<()> = Mutex::const_new(());

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/inventory/photos", get(list))
        .route("/inventory/{id}/photo", get(photo))
        .merge(Router::new().route("/inventory/{id}/photo", post(upload))
            .route_layer(middleware::from_fn_with_state(state, auth::require_admin)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload { image_data_url: String }

#[derive(Serialize, Deserialize)]
struct PhotoMeta { item_id: Uuid, version: Uuid, mime: String }

fn root() -> PathBuf {
    std::env::var_os("INVENTORY_IMAGES_DIR").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/nv-product-images"))
}

fn decode(encoded: &str) -> Result<(String, Vec<u8>), AppError> {
    let invalid = || AppError::BadRequest("사진은 JPG·PNG·WebP, 1MB 이하로 등록해 주세요.".into());
    if encoded.len() > MAX_BYTES.div_ceil(3) * 4 + 32 { return Err(invalid()); }
    let (mime, value) = ["image/jpeg", "image/png", "image/webp"].into_iter()
        .find_map(|mime| encoded.strip_prefix(&format!("data:{mime};base64,")).map(|value| (mime, value)))
        .ok_or_else(invalid)?;
    let data = STANDARD.decode(value).map_err(|_| invalid())?;
    let valid = match mime {
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
        _ => false,
    };
    if !valid || data.len() > MAX_BYTES { return Err(invalid()); }
    Ok((mime.to_owned(), data))
}

async fn metadata(id: Uuid) -> Result<PhotoMeta, AppError> {
    let bytes = tokio::fs::read(root().join(format!("{id}.json"))).await.map_err(|e| {
        if e.kind() == ErrorKind::NotFound { AppError::NotFound } else { AppError::Io(e) }
    })?;
    serde_json::from_slice(&bytes).map_err(anyhow::Error::from).map_err(AppError::from)
}

async fn list() -> Result<Json<Vec<PhotoMeta>>, AppError> {
    let mut dir = match tokio::fs::read_dir(root()).await {
        Ok(dir) => dir,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Json(Vec::new())),
        Err(e) => return Err(e.into()),
    };
    let mut photos = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        if entry.path().extension().is_some_and(|e| e == "json") {
            let bytes = tokio::fs::read(entry.path()).await?;
            photos.push(serde_json::from_slice(&bytes).map_err(anyhow::Error::from)?);
        }
    }
    Ok(Json(photos))
}

async fn photo(Path(id): Path<Uuid>) -> Result<Response, AppError> {
    // Keep the published binary alive until read completes during replacement.
    let _guard = UPLOAD.lock().await;
    let meta = metadata(id).await?;
    let data = tokio::fs::read(root().join(format!("{id}-{}.bin", meta.version))).await?;
    Ok(([(header::CONTENT_TYPE, meta.mime), (header::CACHE_CONTROL, "no-store".into()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into())], data).into_response())
}

async fn upload(State(state): State<AppState>, Path(id): Path<Uuid>, Json(body): Json<Upload>) -> Result<Json<PhotoMeta>, AppError> {
    let (mime, data) = decode(&body.image_data_url)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM neurovector.inventory_items WHERE id=$1)")
        .bind(id).fetch_one(&state.pool).await?;
    if !exists { return Err(AppError::NotFound); }
    let _guard = UPLOAD.lock().await;
    let previous = match metadata(id).await { Ok(meta) => Some(meta), Err(AppError::NotFound) => None, Err(e) => return Err(e) };
    let meta = PhotoMeta { item_id: id, version: Uuid::new_v4(), mime };
    let directory = root();
    tokio::fs::create_dir_all(&directory).await?;
    tokio::fs::write(directory.join(format!("{id}-{}.bin",meta.version)), data).await?;
    let temporary = directory.join(format!("{id}-{}.tmp",meta.version));
    tokio::fs::write(&temporary, serde_json::to_vec(&meta).map_err(anyhow::Error::from)?).await?;
    tokio::fs::rename(temporary,directory.join(format!("{id}.json"))).await?;
    if let Some(old) = previous {
        if let Err(error) = tokio::fs::remove_file(directory.join(format!("{id}-{}.bin",old.version))).await {
            if error.kind() != ErrorKind::NotFound { tracing::warn!(%error, "old product photo cleanup failed"); }
        }
    }
    Ok(Json(meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_or_mismatched_photo_payloads() {
        for data in ["data:image/svg+xml;base64,PHN2Zz4=", "data:image/png;base64,!!!", "data:image/jpeg;base64,YWJj"] {
            assert!(decode(data).is_err());
        }
        let png = format!("data:image/png;base64,{}", STANDARD.encode(b"\x89PNG\r\n\x1a\nbytes"));
        assert_eq!(decode(&png).unwrap().0,"image/png");
    }

    #[test]
    fn accepts_supported_signatures_when_mime_matches() {
        // Given: supported image headers, When: encoded as data URLs.
        for (mime, bytes) in [
            ("image/jpeg", &[0xff, 0xd8, 0xff][..]),
            ("image/png", &b"\x89PNG\r\n\x1a\n"[..]),
            ("image/webp", &b"RIFF\x00\x00\x00\x00WEBP"[..]),
        ] {
            let encoded = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
            // Then: decoded bytes and MIME are preserved.
            assert_eq!(decode(&encoded).unwrap(), (mime.to_owned(), bytes.to_vec()));
        }
    }

    #[test]
    fn accepts_limit_when_decoded_image_is_exactly_one_megabyte() {
        // Given: a PNG with the maximum allowed byte count.
        let mut bytes = vec![0; MAX_BYTES];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        // When: the data URL is decoded.
        let result = decode(&format!("data:image/png;base64,{}", STANDARD.encode(&bytes)));
        // Then: the entire allowed image is retained.
        assert_eq!(result.unwrap().1.len(), MAX_BYTES);
    }

    #[test]
    fn rejects_image_when_decoded_bytes_exceed_limit() {
        // Given: a PNG one byte above the allowed limit.
        let mut bytes = vec![0; MAX_BYTES + 1];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        // When: the data URL is decoded. Then: it is rejected.
        assert!(decode(&format!("data:image/png;base64,{}", STANDARD.encode(&bytes))).is_err());
    }

    #[test]
    fn rejects_payload_when_empty_or_missing_webp_signature() {
        // Given: empty and incomplete signatures. When: decoded. Then: rejected.
        for encoded in ["data:image/png;base64,", "data:image/webp;base64,UklGRg==", "data:image/png;base64,/9j/"] {
            assert!(decode(encoded).is_err());
        }
    }

    async fn test_server() -> (String, tokio::task::JoinHandle<()>) {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused").unwrap();
        let state = crate::state::test_state(pool);
        let app = router(state.clone()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn rejects_upload_when_admin_cookie_is_absent() {
        // Given: the real route and middleware with no database connection.
        let (base, task) = test_server().await;
        // When: an anonymous client posts a photo payload.
        let result = reqwest::Client::new().post(format!("{base}/inventory/{}/photo", Uuid::new_v4()))
            .json(&serde_json::json!({"image_data_url": "data:image/png;base64,iVBORw0KGgo="}))
            .send().await.unwrap();
        task.abort();
        // Then: middleware denies access before database or disk work.
        assert_eq!(result.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn public_photo_read_needs_no_admin_cookie() {
        // Given: the real public photo route and a nonexistent random photo.
        let (base, task) = test_server().await;
        // When: a customer without an admin cookie requests its bytes.
        let result = reqwest::get(format!("{base}/inventory/{}/photo", Uuid::new_v4())).await.unwrap();
        task.abort();
        // Then: the public route returns missing photo, rather than auth denial.
        assert_eq!(result.status(), reqwest::StatusCode::NOT_FOUND);
    }
}
