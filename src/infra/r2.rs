use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use crate::config::AppConfig;
use crate::errors::AppError;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct R2Service {
    account_id: String,
    access_key_id: String,
    secret_access_key: String,
    bucket_name: String,
    public_url: String,
}

impl R2Service {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            account_id: config.r2_account_id.clone(),
            access_key_id: config.r2_access_key_id.clone(),
            secret_access_key: config.r2_secret_access_key.clone(),
            bucket_name: config.r2_bucket_name.clone(),
            public_url: config.r2_public_url.clone(),
        }
    }

    pub fn get_public_url(&self, object_key: &str) -> String {
        if !self.public_url.is_empty() {
            format!("{}/{}", self.public_url.trim_end_matches('/'), object_key)
        } else {
            format!(
                "https://{}.r2.cloudflarestorage.com/{}/{}",
                self.account_id, self.bucket_name, object_key
            )
        }
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Generates AWS SigV4 Presigned PUT URL for Cloudflare R2
    pub fn generate_presigned_put_url(
        &self,
        object_key: &str,
        _content_type: &str,
        expires_seconds: u64,
    ) -> Result<String, AppError> {
        if self.access_key_id.is_empty() || self.secret_access_key.is_empty() {
            // Local dev fallback
            return Ok(format!("http://localhost:4000/v1/mock-upload/{}", object_key));
        }

        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let region = "auto";
        let service = "s3";

        let host = format!("{}.r2.cloudflarestorage.com", self.account_id);
        let canonical_uri = format!("/{}/{}", self.bucket_name, object_key);

        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);
        let x_amz_credential = format!("{}/{}", self.access_key_id, credential_scope);

        // Query params in alphabetical order
        let mut query_params = vec![
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", x_amz_credential),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", expires_seconds.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
        ];
        query_params.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_headers = format!("host:{}\n", host);
        let signed_headers = "host";
        let payload_hash = "UNSIGNED-PAYLOAD";

        let canonical_request = format!(
            "PUT\n{}\n{}\n{}\n{}\n{}",
            canonical_uri, canonical_query_string, canonical_headers, signed_headers, payload_hash
        );

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let hashed_canonical_request = hex::encode(hasher.finalize());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, hashed_canonical_request
        );

        // Calculate signature
        let k_date = Self::hmac_sha256(format!("AWS4{}", self.secret_access_key).as_bytes(), date_stamp.as_bytes());
        let k_region = Self::hmac_sha256(&k_date, region.as_bytes());
        let k_service = Self::hmac_sha256(&k_region, service.as_bytes());
        let k_signing = Self::hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(Self::hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let final_url = format!(
            "https://{}{}?{}&X-Amz-Signature={}",
            host, canonical_uri, canonical_query_string, signature
        );

        Ok(final_url)
    }

    /// Upload bytes directly from worker (e.g. WebP thumbnail)
    pub async fn upload_bytes(
        &self,
        object_key: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<String, AppError> {
        let presigned_url = self.generate_presigned_put_url(object_key, content_type, 300)?;
        let client = reqwest::Client::new();

        client
            .put(&presigned_url)
            .header("Content-Type", content_type)
            .body(bytes)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to upload to R2: {}", e)))?;

        Ok(self.get_public_url(object_key))
    }
}
