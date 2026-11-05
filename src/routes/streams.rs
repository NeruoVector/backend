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

#[derive(Debug, Serialize, utoipa::ToSchema)]
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

#[utoipa::path(get,path="/api/v1/streams",tag="streams",responses((status=200,description="MediaMTX 스트림 목록",body=serde_json::Value),(status=503,description="미디어 서버 연결 실패")))]
pub(crate) async fn list_streams(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let paths = state
        .media
        .list_paths()
        .await
        .map_err(|err| AppError::ServiceUnavailable(err.to_string()))?;
    Ok(Json(paths))
}

#[utoipa::path(get,path="/api/v1/streams/{drone_id}",tag="streams",params(("drone_id"=String,Path,example="school-drone-001")),responses((status=200,description="SRT 송출·수신 주소와 WebRTC WHEP 경로",body=StreamInfo),(status=400,description="잘못된 드론 ID")))]
pub(crate) async fn stream_info(
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

#[utoipa::path(post,path="/api/v1/streams/{drone_id}/whep",tag="streams",params(("drone_id"=String,Path,example="school-drone-001")),request_body(content=String,content_type="application/sdp",description="RTCPeerConnection.createOffer()로 만든 실제 SDP offer"),responses((status=201,description="SDP answer와 세션 Location",body=String,content_type="application/sdp",headers(("Location"=String,description="DELETE할 백엔드 WHEP 세션 경로"))),(status=400,description="잘못된 SDP offer"),(status=404,description="송출 중인 스트림 없음"),(status=503,description="미디어 서버 연결 실패")),description="브라우저는 프론트엔드 /webrtc-config.json의 ICE/TURN 설정으로 RTCPeerConnection을 만든 뒤 ICE 수집을 마치고 offer를 전송합니다. 응답 SDP를 setRemoteDescription에 적용하고, 종료 시 Location 경로로 DELETE합니다. Swagger에서 단순 GET으로 영상을 재생하는 API가 아닙니다.")]
pub(crate) async fn whep_offer(
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
        let session = location
            .to_str()
            .map_err(anyhow::Error::from)?
            .split('?')
            .next()
            .and_then(|path| path.trim_end_matches('/').rsplit('/').next())
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| {
                AppError::ServiceUnavailable("미디어 서버의 세션 경로가 올바르지 않습니다.".into())
            })?;
        let path = format!("/api/v1/streams/{drone_id}/whep/{session}");
        headers.insert(
            header::LOCATION,
            HeaderValue::from_str(&path).map_err(anyhow::Error::from)?,
        );
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Location, Content-Type"),
    );

    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, headers, answer).into_response())
}

#[utoipa::path(delete,path="/api/v1/streams/{drone_id}/whep/{session_id}",tag="streams",params(("drone_id"=String,Path,example="school-drone-001"),("session_id"=Uuid,Path,description="WHEP 응답 Location의 세션 ID")),responses((status=200,description="미디어 서버가 세션 종료를 확인"),(status=204,description="세션 종료"),(status=404,description="세션 없음")))]
pub(crate) async fn whep_delete(
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

#[utoipa::path(get,path="/api/v1/recordings",tag="streams",params(("drone_id"=Option<String>,Query,description="생략하면 전체 기체 녹화 목록")),responses((status=200,description="녹화 목록",body=Vec<crate::media::Recording>)))]
pub(crate) async fn list_recordings(
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

#[utoipa::path(get,path="/api/v1/recordings/{drone_id}/{filename}",tag="streams",params(("drone_id"=String,Path),("filename"=String,Path)),responses((status=200,description="녹화 파일 다운로드",body=Vec<u8>,content_type="application/octet-stream"),(status=404,description="녹화 파일 없음")))]
pub(crate) async fn download_recording(
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
