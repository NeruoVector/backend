use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::{error::AppError, media::Recording, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/streams", get(list_streams))
        .route("/streams/{drone_id}", get(stream_info))
        .route("/streams/{drone_id}/whep", post(whep_offer))
        .route("/streams/{drone_id}/whep/{session_id}", delete(whep_delete))
        .route("/recordings", get(list_recordings))
        .route("/recordings/{drone_id}/{filename}", get(download_recording))
}

#[derive(Debug, Serialize)]
pub struct StreamInfo {
    pub drone_id: String,
    pub publish_url: String,
    pub read_url: String,
    pub webrtc_whep_path: String,
    pub active: bool,
    pub path: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct RecordingQuery {
    #[serde(default)]
    pub drone_id: Option<String>,
}

async fn list_streams(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let paths = state
        .media
        .list_paths()
        .await
        .map_err(|err| AppError::ServiceUnavailable(err.to_string()))?;
    Ok(Json(paths))
}

async fn stream_info(
    State(state): State<AppState>,
    Path(drone_id): Path<String>,
) -> Result<Json<StreamInfo>, AppError> {
    crate::media::validate_slug(&drone_id).map_err(|err| AppError::BadRequest(err.to_string()))?;

    let paths = state
        .media
        .list_paths()
        .await
        .map_err(|err| AppError::ServiceUnavailable(err.to_string()))?;
    let path = find_path(&paths, &drone_id);
    let urls = state.media.srt_urls(&drone_id);

    Ok(Json(StreamInfo {
        drone_id,
        publish_url: urls.publish_url,
        read_url: urls.read_url,
        webrtc_whep_path: urls.webrtc_whep_path,
        active: path.is_some(),
        path,
    }))
}

async fn whep_offer(
    State(state): State<AppState>,
    Path(drone_id): Path<String>,
    body: Bytes,
) -> Result<Response, AppError> {
    if body.is_empty() {
        return Err(AppError::BadRequest("WHEP offer SDP is required".into()));
    }
    let offer = String::from_utf8(body.to_vec())
        .map_err(|_| AppError::BadRequest("WHEP offer must be valid UTF-8".into()))?;

    let (status, upstream_headers, answer) = state
        .media
        .proxy_whep(&drone_id, offer)
        .await
        .map_err(|err| AppError::ServiceUnavailable(err.to_string()))?;

    let mut headers = HeaderMap::new();
    if let Some(content_type) = upstream_headers.get(header::CONTENT_TYPE) {
        headers.insert(header::CONTENT_TYPE, content_type.clone());
    } else {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/sdp"),
        );
    }
    if let Some(location) = upstream_headers.get(header::LOCATION) {
        headers.insert(header::LOCATION, location.clone());
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Location, Content-Type"),
    );

    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, headers, answer).into_response())
}

async fn whep_delete(
    State(state): State<AppState>,
    Path((drone_id, session_id)): Path<(String, Uuid)>,
) -> Result<StatusCode, AppError> {
    let status = state
        .media
        .delete_whep(&drone_id, &session_id.to_string())
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    Ok(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY))
}

async fn list_recordings(
    State(state): State<AppState>,
    Query(query): Query<RecordingQuery>,
) -> Result<Json<Vec<Recording>>, AppError> {
    let recordings = match query.drone_id {
        Some(drone_id) => state
            .media
            .list_recordings(&drone_id)
            .await
            .map_err(|err| AppError::BadRequest(err.to_string()))?,
        None => state
            .media
            .list_all_recordings()
            .await
            .map_err(|err| AppError::ServiceUnavailable(err.to_string()))?,
    };

    Ok(Json(recordings))
}

async fn download_recording(
    State(state): State<AppState>,
    Path((drone_id, filename)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let path = state
        .media
        .recording_path(&drone_id, &filename)
        .await
        .map_err(|err| {
            let message = err.to_string();
            if message.contains("No such file") || message.contains("os error 2") {
                AppError::NotFound
            } else {
                AppError::BadRequest(message)
            }
        })?;

    let metadata = tokio::fs::metadata(&path).await?;
    let file = File::open(&path).await?;
    let stream = ReaderStream::new(file);

    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some("mp4") => "video/mp4",
        Some("ts") => "video/mp2t",
        Some("mkv") => "video/x-matroska",
        _ => "application/octet-stream",
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .map_err(|err| AppError::Other(err.into()))?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|err| AppError::Other(err.into()))?,
    );

    Ok((headers, Body::from_stream(stream)))
}

fn find_path(paths: &Value, drone_id: &str) -> Option<Value> {
    paths
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(Value::as_str) == Some(drone_id))
        })
        .cloned()
}
