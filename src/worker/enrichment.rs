use std::cell::RefCell;
use std::rc::Rc;
use lol_html::{element, text, HtmlRewriter, Settings};
use url::Url;

use crate::errors::AppError;
use crate::infra::http_client::SsrfSafeClient;

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtractedMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub favicon_url: Option<String>,
    pub site_name: Option<String>,
}

pub struct MetadataExtractor {
    http_client: SsrfSafeClient,
}

impl MetadataExtractor {
    pub fn new(http_client: SsrfSafeClient) -> Self {
        Self { http_client }
    }

    pub fn parse_html(html_bytes: &[u8], base_url_str: &str) -> Result<ExtractedMetadata, AppError> {
        let base_url = Url::parse(base_url_str).ok();

        let raw_title = Rc::new(RefCell::new(String::new()));
        let og_title = Rc::new(RefCell::new(None::<String>));
        let og_desc = Rc::new(RefCell::new(None::<String>));
        let meta_desc = Rc::new(RefCell::new(None::<String>));
        let og_image = Rc::new(RefCell::new(None::<String>));
        let favicon = Rc::new(RefCell::new(None::<String>));
        let og_site_name = Rc::new(RefCell::new(None::<String>));

        let t_clone = raw_title.clone();
        let og_t_clone = og_title.clone();
        let og_d_clone = og_desc.clone();
        let m_d_clone = meta_desc.clone();
        let og_i_clone = og_image.clone();
        let fav_clone = favicon.clone();
        let site_clone = og_site_name.clone();

        let mut rewriter = HtmlRewriter::new(
            Settings {
                element_content_handlers: vec![
                    text!("title", move |chunk| {
                        t_clone.borrow_mut().push_str(chunk.as_str());
                        Ok(())
                    }),
                    element!("meta", move |el| {
                        let prop = el.get_attribute("property").or_else(|| el.get_attribute("name"));
                        let content = el.get_attribute("content");

                        if let (Some(p), Some(c)) = (prop, content) {
                            let p_lower = p.to_lowercase();
                            let c_trimmed = c.trim().to_string();
                            if !c_trimmed.is_empty() {
                                match p_lower.as_str() {
                                    "og:title" | "twitter:title" => {
                                        if og_t_clone.borrow().is_none() {
                                            *og_t_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    "og:description" | "twitter:description" => {
                                        if og_d_clone.borrow().is_none() {
                                            *og_d_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    "description" => {
                                        if m_d_clone.borrow().is_none() {
                                            *m_d_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    "og:image" | "twitter:image" => {
                                        if og_i_clone.borrow().is_none() {
                                            *og_i_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    "og:site_name" => {
                                        if site_clone.borrow().is_none() {
                                            *site_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Ok(())
                    }),
                    element!("link", move |el| {
                        let rel = el.get_attribute("rel").map(|r| r.to_lowercase());
                        let href = el.get_attribute("href");

                        if let (Some(r), Some(h)) = (rel, href) {
                            if (r.contains("icon") || r.contains("shortcut")) && fav_clone.borrow().is_none() {
                                *fav_clone.borrow_mut() = Some(h.trim().to_string());
                            }
                        }
                        Ok(())
                    }),
                ],
                ..Settings::default()
            },
            |_: &[u8]| {},
        );

        let _ = rewriter.write(html_bytes);
        let _ = rewriter.end();

        let title = og_title
            .borrow()
            .clone()
            .or_else(|| {
                let t = raw_title.borrow().trim().to_string();
                if !t.is_empty() {
                    Some(t)
                } else {
                    None
                }
            });

        let description = og_desc.borrow().clone().or_else(|| meta_desc.borrow().clone());

        let resolve_url = |val: Option<String>| -> Option<String> {
            let s = val?;
            if let Some(base) = &base_url {
                base.join(&s).ok().map(|u| u.to_string())
            } else {
                Some(s)
            }
        };

        let image_url = resolve_url(og_image.borrow().clone());
        let mut favicon_url = resolve_url(favicon.borrow().clone());

        // Default favicon fallback to /favicon.ico if host is known
        if favicon_url.is_none() {
            if let Some(base) = &base_url {
                if let Ok(fav) = base.join("/favicon.ico") {
                    favicon_url = Some(fav.to_string());
                }
            }
        }

        let site_name = og_site_name.borrow().clone();

        Ok(ExtractedMetadata {
            title,
            description,
            image_url,
            favicon_url,
            site_name,
        })
    }

    /// Fetches URL safely with SSRF protection, streaming up to 1MB, stopping at </head>
    pub async fn fetch_and_extract(&self, target_url: &str) -> Result<ExtractedMetadata, AppError> {
        let mut response = self.http_client.get_safe(target_url).await?;

        // 1. Check content-type header
        if let Some(content_type) = response.headers().get(reqwest::header::CONTENT_TYPE) {
            let ct_str = content_type.to_str().unwrap_or_default().to_lowercase();
            if !ct_str.contains("text/html") && !ct_str.contains("application/xhtml") {
                // Not an HTML document (e.g. PDF, image, audio, zip)
                return Ok(ExtractedMetadata {
                    title: None,
                    description: Some(format!("Content-Type: {}", ct_str)),
                    image_url: None,
                    favicon_url: None,
                    site_name: None,
                });
            }
        }

        let max_stream_bytes = 1024 * 1024; // 1 MB limit
        let mut buffer = Vec::with_capacity(64 * 1024);

        while let Some(chunk) = response.chunk().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
            buffer.extend_from_slice(&chunk);

            // Check if </head> has arrived, or if buffer exceeded max limit
            if buffer.len() >= max_stream_bytes {
                break;
            }

            // Early termination check for </head>
            if let Ok(s) = std::str::from_utf8(&buffer[buffer.len().saturating_sub(1024)..]) {
                if s.to_lowercase().contains("</head>") {
                    break;
                }
            }
        }

        Self::parse_html(&buffer, target_url)
    }
}
