//! "Download media" for saved posts on X, TikTok, Instagram and Vimeo, via yt-dlp (+ ffmpeg).
//! YouTube is deliberately unsupported: its terms forbid it and it blocks server IPs.
use std::{path::PathBuf, process::Stdio, sync::LazyLock, time::Duration};

use axum::{
    body::Body,
    extract::Query,
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Semaphore};
use utoipa::ToSchema;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;

const MAX_DURATION_SECS: f64 = 20.0 * 60.0;
const MAX_FILESIZE: &str = "300M";
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(45);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(240);
/// A download holds a yt-dlp + ffmpeg process and a temp file; keep the small VPS responsive.
static SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));

const PLATFORMS: [(&str, &str); 5] = [
    ("x.com", "X"),
    ("twitter.com", "X"),
    ("tiktok.com", "TikTok"),
    ("instagram.com", "Instagram"),
    ("vimeo.com", "Vimeo"),
];

/// Only public posts on the supported platforms; anything else would let yt-dlp fetch arbitrary URLs (SSRF).
fn platform_for(raw: &str) -> Result<&'static str, AppError> {
    let url = url::Url::parse(raw).map_err(|_| AppError::BadRequest("Invalid URL".into()))?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let matches = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if matches("youtube.com") || matches("youtu.be") {
        return Err(AppError::BadRequest("YouTube videos can't be downloaded. Open them on YouTube instead.".into()));
    }
    let plain = matches!(url.scheme(), "https" | "http") && url.username().is_empty() && url.password().is_none() && url.port().is_none();
    PLATFORMS
        .iter()
        .find(|(domain, _)| plain && matches(domain))
        .map(|(_, name)| *name)
        .ok_or_else(|| AppError::BadRequest("Downloads work for posts on X, TikTok, Instagram and Vimeo".into()))
}

/// yt-dlp format ids are short tokens like "hls-720" or "http-1080p"; refuse anything else.
fn valid_format_id(id: &str) -> bool {
    // No leading "-", so an id can never read as a yt-dlp option.
    !id.is_empty() && id.len() <= 64 && !id.starts_with('-') && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

async fn yt_dlp(args: &[&str], timeout: Duration) -> Result<Vec<u8>, AppError> {
    let child = Command::new("yt-dlp")
        // Ignore any config file on the host and never follow playlists.
        .args(["--ignore-config", "--no-playlist", "--no-warnings", "--no-progress"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("yt-dlp could not start: {e}")))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| AppError::BadRequest("The platform took too long to respond. Try again later.".into()))?
        .map_err(|e| AppError::Internal(e.into()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr.chars().take(500).collect::<String>(), "yt-dlp failed");
        return Err(AppError::BadRequest("Could not read media from this post. It may be private or removed.".into()));
    }
    Ok(output.stdout)
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ExtractQuery {
    pub url: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaFormat {
    pub format_id: String,
    pub ext: String,
    pub resolution: String,
    pub filesize: Option<u64>,
    pub note: String,
    pub has_video: bool,
    pub has_audio: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub title: String,
    pub author: Option<String>,
    pub duration: Option<u64>,
    pub thumbnail: Option<String>,
    pub platform: String,
    pub formats: Vec<MediaFormat>,
}

fn media_info(info: &serde_json::Value, platform: &str) -> MediaInfo {
    let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let formats = info["formats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let format_id = f["format_id"].as_str().filter(|id| valid_format_id(id))?.to_owned();
            let has_video = f["vcodec"].as_str().is_some_and(|c| c != "none");
            let has_audio = f["acodec"].as_str().is_some_and(|c| c != "none");
            Some(MediaFormat {
                resolution: if has_video { text(&f["resolution"]).unwrap_or_else(|| "Video".into()) } else { "Audio only".into() },
                ext: text(&f["ext"]).unwrap_or_default(),
                filesize: f["filesize"].as_u64().or_else(|| f["filesize_approx"].as_u64()),
                note: text(&f["format_note"]).unwrap_or_default(),
                format_id,
                has_video,
                has_audio,
            })
        })
        // Best first, as the dialog preselects the first entry.
        .rev()
        .collect();
    MediaInfo {
        title: text(&info["title"]).unwrap_or_else(|| "Untitled video".into()),
        author: text(&info["uploader"]).or_else(|| text(&info["channel"])),
        duration: info["duration"].as_f64().map(|d| d.round() as u64),
        thumbnail: text(&info["thumbnail"]),
        platform: platform.into(),
        formats,
    }
}

/// Reads one post's title, length and formats (yt-dlp lists formats worst to best).
async fn inspect(url: &str) -> Result<(serde_json::Value, &'static str), AppError> {
    let platform = platform_for(url)?;
    let stdout = yt_dlp(&["-J", "--", url], EXTRACT_TIMEOUT).await?;
    let mut info: serde_json::Value = serde_json::from_slice(&stdout).map_err(|e| AppError::Internal(e.into()))?;
    // A post with several videos comes back as a playlist; the first one is what the card shows.
    if info["_type"] == "playlist" {
        info = info["entries"][0].take();
    }
    if info["duration"].as_f64().is_some_and(|d| d > MAX_DURATION_SECS) {
        return Err(AppError::BadRequest("Videos longer than 20 minutes can't be downloaded".into()));
    }
    Ok((info, platform))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/media/extract",
    params(ExtractQuery),
    responses((status = 200, description = "Title and formats of a post's video", body = MediaInfo)),
    tag = "bookmarks"
)]
pub async fn extract_media(
    CurrentUser(_user): CurrentUser,
    Query(query): Query<ExtractQuery>,
) -> Result<Json<MediaInfo>, AppError> {
    let (info, platform) = inspect(&query.url).await?;
    Ok(Json(media_info(&info, platform)))
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadMediaRequest {
    pub url: String,
    pub format_id: Option<String>,
    /// VIDEO (MP4) or AUDIO (MP3).
    pub media_type: String,
}

/// Removes the temp directory when the request ends, whatever happens.
struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[utoipa::path(
    post,
    path = "/v1/bookmarks/media/download",
    request_body = DownloadMediaRequest,
    responses((status = 200, description = "The MP4 or MP3 file", content_type = "application/octet-stream")),
    tag = "bookmarks"
)]
pub async fn download_media(
    CurrentUser(_user): CurrentUser,
    Json(req): Json<DownloadMediaRequest>,
) -> Result<Response, AppError> {
    let audio = match req.media_type.as_str() {
        "AUDIO" => true,
        "VIDEO" => false,
        _ => return Err(AppError::BadRequest("mediaType must be VIDEO or AUDIO".into())),
    };
    if req.format_id.as_deref().is_some_and(|id| !valid_format_id(id)) {
        return Err(AppError::BadRequest("Invalid format".into()));
    }
    let _slot = SLOTS
        .try_acquire()
        .map_err(|_| AppError::TooManyRequests("Other downloads are running. Try again in a minute.".into()))?;
    // Checks the platform and the length before spending time and disk on the file.
    inspect(&req.url).await?;

    let dir = TempDir(std::env::temp_dir().join(format!("cabinet-media-{}", uuid::Uuid::now_v7())));
    std::fs::create_dir(&dir.0).map_err(|e| AppError::Internal(e.into()))?;
    let (ext, content_type) = if audio { ("mp3", "audio/mpeg") } else { ("mp4", "video/mp4") };
    let output = dir.0.join(format!("media.{ext}"));
    let template = dir.0.join("media.%(ext)s");
    let format = match (&req.format_id, audio) {
        (Some(id), true) => id.clone(),
        (None, true) => "bestaudio/best".into(),
        // A video-only stream gets the best audio merged in; one that has audio is used as is.
        (Some(id), false) => format!("{id}[acodec=none]+bestaudio/{id}"),
        (None, false) => "bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best".into(),
    };
    let template = template.to_string_lossy();
    let mut args = vec!["-f", format.as_str(), "--max-filesize", MAX_FILESIZE, "-o", &*template];
    if audio {
        args.extend(["-x", "--audio-format", "mp3"]);
    } else {
        args.extend(["--merge-output-format", "mp4", "--remux-video", "mp4"]);
    }
    args.extend(["--", req.url.as_str()]);
    yt_dlp(&args, DOWNLOAD_TIMEOUT).await?;

    // yt-dlp exits successfully without a file when it skips one over --max-filesize.
    let file = tokio::fs::File::open(&output)
        .await
        .map_err(|_| AppError::BadRequest("This video is larger than 300 MB and can't be downloaded".into()))?;
    let size = file.metadata().await.map_err(|e| AppError::Internal(e.into()))?.len();
    // The open handle keeps the data readable after the directory is removed (TempDir drops here).
    drop(dir);
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"media.{ext}\"")),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_platforms_reach_yt_dlp() {
        for (url, platform) in [
            ("https://x.com/user/status/123", "X"),
            ("https://mobile.twitter.com/user/status/123", "X"),
            ("https://www.tiktok.com/@a/video/1", "TikTok"),
            ("https://www.instagram.com/reel/abc/", "Instagram"),
            ("https://vimeo.com/123", "Vimeo"),
        ] {
            assert_eq!(platform_for(url).unwrap(), platform, "{url}");
        }
        for url in [
            "https://www.youtube.com/watch?v=x",
            "https://youtu.be/x",
            "https://example.com/video.mp4",
            "https://x.com.evil.test/a",
            "https://notx.com/a",
            "file:///etc/passwd",
            "https://user:pw@x.com/a",
            "https://x.com:8443/a",
            "http://169.254.169.254/latest",
            "-x",
        ] {
            assert!(platform_for(url).is_err(), "{url}");
        }
        assert!(platform_for("https://youtu.be/x").unwrap_err().to_string().contains("YouTube"));
    }

    #[test]
    fn format_ids_are_plain_tokens() {
        for id in ["hls-720", "http-1080p", "0", "dash_video.1"] {
            assert!(valid_format_id(id), "{id}");
        }
        for id in ["", "best+bestaudio", "a b", "--exec", "x[height>1]", &"a".repeat(65)] {
            assert!(!valid_format_id(id), "{id}");
        }
    }

    #[test]
    fn formats_come_best_first_without_unusable_ids() {
        let info = serde_json::json!({
            "title": "Clip", "uploader": "someone", "duration": 12.4,
            "formats": [
                { "format_id": "audio", "vcodec": "none", "acodec": "mp4a", "ext": "m4a" },
                { "format_id": "bad id", "vcodec": "avc1", "acodec": "none" },
                { "format_id": "hls-720", "vcodec": "avc1", "acodec": "none", "resolution": "1280x720", "filesize_approx": 10 },
            ]
        });
        let media = media_info(&info, "X");
        let ids: Vec<_> = media.formats.iter().map(|f| f.format_id.as_str()).collect();
        assert_eq!(ids, ["hls-720", "audio"]);
        assert_eq!(media.formats[0].filesize, Some(10));
        assert_eq!(media.formats[1].resolution, "Audio only");
        assert_eq!(media.duration, Some(12));
    }
}
