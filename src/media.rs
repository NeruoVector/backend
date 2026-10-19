use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use reqwest::{StatusCode, header::HeaderMap};
use serde::Serialize;

/// MediaMTX 와 SRT/WebRTC/녹화 파일을 다루는 클라이언트.
#[derive(Clone)]
pub struct MediaClient {
    http: reqwest::Client,
    api_base_url: String,
    whep_base_url: String,
    srt_public_host: String,
    srt_port: u16,
    recordings_dir: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct SrtUrls {
    pub publish_url: String,
    pub read_url: String,
    pub webrtc_whep_path: String,
}

#[derive(Debug, Serialize)]
pub struct Recording {
    pub drone_id: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub download_url: String,
}

impl MediaClient {
    pub fn new(
        api_base_url: impl Into<String>,
        whep_base_url: impl Into<String>,
        srt_public_host: impl Into<String>,
        srt_port: u16,
        recordings_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_base_url: api_base_url.into().trim_end_matches('/').to_string(),
            whep_base_url: whep_base_url.into().trim_end_matches('/').to_string(),
            srt_public_host: srt_public_host.into(),
            srt_port,
            recordings_dir: recordings_dir.into(),
        }
    }

    pub fn srt_urls(&self, drone_id: &str) -> SrtUrls {
        let host = &self.srt_public_host;
        let port = self.srt_port;
        SrtUrls {
            publish_url: format!("srt://{host}:{port}?streamid=publish:{drone_id}"),
            read_url: format!("srt://{host}:{port}?streamid=read:{drone_id}"),
            webrtc_whep_path: format!("/api/v1/streams/{drone_id}/whep"),
        }
    }

    pub async fn list_paths(&self) -> anyhow::Result<serde_json::Value> {
        let response = self
            .http
            .get(format!("{}/v3/paths/list", self.api_base_url))
            .send()
            .await
            .context("MediaMTX API request failed")?
            .error_for_status()?;

        Ok(response.json().await?)
    }

    /// WHEP offer 를 MediaMTX 로 전달하고 answer 를 그대로 돌려준다.
    pub async fn proxy_whep(
        &self,
        drone_id: &str,
        offer_sdp: String,
    ) -> anyhow::Result<(StatusCode, HeaderMap, String)> {
        validate_slug(drone_id)?;

        let response = self
            .http
            .post(format!("{}/{drone_id}/whep", self.whep_base_url))
            .header(reqwest::header::CONTENT_TYPE, "application/sdp")
            .body(offer_sdp)
            .send()
            .await
            .context("MediaMTX WHEP request failed")?;

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await?;
        Ok((status, headers, body))
    }

    pub async fn delete_whep(
        &self,
        drone_id: &str,
        session_id: &str,
    ) -> anyhow::Result<StatusCode> {
        validate_slug(drone_id)?;
        uuid::Uuid::parse_str(session_id).context("invalid WHEP session id")?;

        let response = self
            .http
            .delete(format!(
                "{}/{drone_id}/whep/{session_id}",
                self.whep_base_url
            ))
            .send()
            .await
            .context("MediaMTX WHEP delete failed")?;

        Ok(response.status())
    }

    pub async fn list_recordings(&self, drone_id: &str) -> anyhow::Result<Vec<Recording>> {
        validate_slug(drone_id)?;
        let dir = self.recordings_dir.join(drone_id);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };

        let mut recordings = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if !matches!(
                path.extension().and_then(|v| v.to_str()),
                Some("mp4" | "ts" | "mkv" | "m4s")
            ) {
                continue;
            }
            let metadata = entry.metadata().await?;
            let modified_at = metadata.modified().ok().map(|time| {
                let datetime: chrono::DateTime<chrono::Utc> = time.into();
                datetime.to_rfc3339()
            });

            recordings.push(Recording {
                drone_id: drone_id.to_string(),
                name: name.to_string(),
                size_bytes: metadata.len(),
                modified_at,
                download_url: format!("/api/v1/recordings/{drone_id}/{name}"),
            });
        }

        recordings.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
        Ok(recordings)
    }

    pub async fn list_all_recordings(&self) -> anyhow::Result<Vec<Recording>> {
        let mut entries = match tokio::fs::read_dir(&self.recordings_dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };

        let mut recordings = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let Some(drone_id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if validate_slug(&drone_id).is_err() {
                continue;
            }
            recordings.extend(self.list_recordings(&drone_id).await?);
        }

        recordings.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
        Ok(recordings)
    }

    /// 경로 traversal 을 막으면서 녹화 파일의 실제 경로를 반환한다.
    pub async fn recording_path(&self, drone_id: &str, filename: &str) -> anyhow::Result<PathBuf> {
        validate_slug(drone_id)?;
        let file_path = Path::new(filename);
        if file_path.components().count() != 1
            || !matches!(file_path.components().next(), Some(Component::Normal(_)))
        {
            bail!("invalid recording filename");
        }

        let root = tokio::fs::canonicalize(&self.recordings_dir).await?;
        let candidate = self.recordings_dir.join(drone_id).join(filename);
        let canonical = tokio::fs::canonicalize(&candidate).await?;
        if !canonical.starts_with(&root) {
            bail!("recording path escapes recordings directory");
        }
        Ok(canonical)
    }
}

/// MediaMTX path / drone_id 로 사용할 수 있는 짧은 slug 인지 확인.
pub fn validate_slug(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("invalid id: only ASCII alphanumeric, '-' and '_' are allowed");
    }
    Ok(())
}
