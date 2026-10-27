use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::info;

use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;
use crate::infra::r2::R2Service;

pub struct ThumbnailProcessor {
    http_client: SsrfSafeClient,
    r2: R2Service,
    semaphore: Arc<Semaphore>,
}

impl ThumbnailProcessor {
    pub fn new(r2: R2Service, semaphore: Arc<Semaphore>) -> Result<Self, anyhow::Error> {
        let http_client = SsrfSafeClient::new()?;
        Ok(Self {
            http_client,
            r2,
            semaphore,
        })
    }

    /// Resize image bytes to WebP with max 640px dimension
    pub fn compress_to_webp(image_bytes: &[u8], max_dim: u32) -> Result<Vec<u8>, AppError> {
        let img = image::load_from_memory(image_bytes)
            .map_err(|e| AppError::BadRequest(format!("Image decode failed: {}", e)))?;

        let resized = img.thumbnail(max_dim, max_dim);

        let mut output_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut output_bytes);

        resized
            .write_to(&mut cursor, image::ImageFormat::WebP)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("WebP encode failed: {}", e)))?;

        Ok(output_bytes)
    }

    /// Safely downloads remote image, resizes under semaphore lock, and uploads WebP to R2
    pub async fn process_and_upload_thumbnail(
        &self,
        image_url: &str,
        url_hash: &[u8],
    ) -> Result<String, AppError> {
        // 1. Download image safely with SSRF protection
        let mut resp = self.http_client.get_safe(image_url).await?;

        let max_download_size = 5 * 1024 * 1024; // 5 MB max source image
        let mut raw_bytes = Vec::with_capacity(256 * 1024);

        while let Some(chunk) = resp.chunk().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
            raw_bytes.extend_from_slice(&chunk);
            if raw_bytes.len() > max_download_size {
                return Err(AppError::BadRequest("Source image exceeds 5 MB limit".to_string()));
            }
        }

        // 2. Acquire memory semaphore to protect VPS 2GB RAM from image decode spikes
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Semaphore error: {}", e)))?;

        info!(image_url = %image_url, "Resizing image to 640px WebP");
        let webp_bytes = Self::compress_to_webp(&raw_bytes, 640)?;

        // 3. Upload to Cloudflare R2
        let hash_hex = hex::encode(url_hash);
        let object_key = format!("thumbs/{}.webp", hash_hex);

        let public_url = self
            .r2
            .upload_bytes(&object_key, "image/webp", webp_bytes)
            .await?;

        Ok(public_url)
    }
}
