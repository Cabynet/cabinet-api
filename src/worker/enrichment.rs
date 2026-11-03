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

type OembedTuple = (Option<String>, Option<String>, Option<String>, Option<String>);

impl MetadataExtractor {
    pub fn new(http_client: SsrfSafeClient) -> Self {
        Self { http_client }
    }

    fn decode_html_entities(input: &str) -> String {
        input
            .replace("&amp;", "&")
            .replace("&#38;", "&")
            .replace("&quot;", "\"")
            .replace("&#34;", "\"")
            .replace("&apos;", "'")
            .replace("&#39;", "'")
            .replace("&#x27;", "'")
            .replace("&#x2019;", "’")
            .replace("&#8217;", "’")
            .replace("&#8211;", "–")
            .replace("&#8212;", "—")
            .replace("&ndash;", "–")
            .replace("&mdash;", "—")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&nbsp;", " ")
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
                                    "og:image" | "og:image:url" | "og:image:secure_url" | "twitter:image" | "twitter:image:src" => {
                                        if og_i_clone.borrow().is_none() {
                                            *og_i_clone.borrow_mut() = Some(c_trimmed);
                                        }
                                    }
                                    "og:site_name" if site_clone.borrow().is_none() => {
                                        *site_clone.borrow_mut() = Some(c_trimmed);
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
            title: title.map(|t| Self::decode_html_entities(&t)),
            description: description.map(|d| Self::decode_html_entities(&d)),
            image_url,
            favicon_url,
            site_name: site_name.map(|s| Self::decode_html_entities(&s)),
        })
    }

    fn is_challenge_title(title: Option<&str>) -> bool {
        let Some(t) = title else { return true };
        let lower = t.trim().to_ascii_lowercase();
        lower.is_empty()
            || lower.starts_with("just a moment")
            || lower.starts_with("attention required")
            || lower.starts_with("access denied")
            || lower.contains("cloudflare")
    }

    async fn fetch_oembed(&self, target_url: &str) -> Option<OembedTuple> {
        let parsed = Url::parse(target_url).ok()?;
        let host = parsed.host_str()?.to_ascii_lowercase();
        let encoded = urlencoding::encode(target_url);
        let endpoint = if host.contains("reddit.com") || host.contains("redd.it") {
            format!("https://www.reddit.com/oembed?url={encoded}")
        } else if host.contains("youtube.com") || host.contains("youtu.be") {
            format!("https://www.youtube.com/oembed?url={encoded}&format=json")
        } else if host.contains("vimeo.com") {
            format!("https://vimeo.com/api/oembed.json?url={encoded}")
        } else {
            return None;
        };

        let resp = self
            .http_client
            .get_safe_with_ua(&endpoint, Some("WhatsApp/2.24.6.77 A"))
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().await.ok()?;
        let title = json["title"].as_str().map(|s| Self::decode_html_entities(s.trim())).filter(|s| !s.is_empty());
        let author = json["author_name"].as_str().map(|s| Self::decode_html_entities(s.trim())).filter(|s| !s.is_empty());
        let thumb = json["thumbnail_url"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let provider = json["provider_name"].as_str().map(|s| Self::decode_html_entities(s.trim())).filter(|s| !s.is_empty());
        Some((title, author, thumb, provider))
    }

    fn apply_oembed_and_reddit_fallbacks(
        &self,
        mut meta: ExtractedMetadata,
        target_url: &str,
        oembed: Option<OembedTuple>,
    ) -> ExtractedMetadata {
        let is_reddit = target_url.contains("reddit.com") || target_url.contains("redd.it");
        if let Some((oe_title, oe_author, oe_thumb, oe_provider)) = oembed {
            let generic_reddit_title = meta.title.as_deref().is_some_and(|t| {
                t.contains("community on Reddit") || t.contains("komunitas") || t.contains("404 Error")
            });
            if oe_title.is_some() && (meta.title.is_none() || generic_reddit_title) {
                meta.title = oe_title;
            }
            if is_reddit {
                if let Some(author) = oe_author {
                    meta.description = Some(format!("Posted by u/{author}"));
                }
            } else if meta.description.as_deref().is_some_and(|d| d.contains("Explore this post") || d.contains("Jelajahi postingan ini")) {
                meta.description = oe_author.map(|a| format!("By {a}"));
            }
            if meta.image_url.is_none() {
                meta.image_url = oe_thumb;
            }
            if meta.site_name.is_none() {
                meta.site_name = oe_provider;
            }
        }
        if is_reddit && meta.image_url.is_none() {
            if let Ok(u) = Url::parse(target_url) {
                let segs: Vec<&str> = u.path_segments().map(|c| c.collect()).unwrap_or_default();
                if let Some(idx) = segs.iter().position(|s| *s == "comments") {
                    if let Some(post_id) = segs.get(idx + 1).filter(|s| !s.is_empty()) {
                        meta.image_url = Some(format!("https://share.redd.it/preview/post/{post_id}"));
                    }
                }
            }
        }
        meta
    }

    /// Fetches URL safely with SSRF protection, crawler UAs, and headless browser fallback
    pub async fn fetch_and_extract(&self, target_url: &str) -> Result<ExtractedMetadata, AppError> {
        let oembed = self.fetch_oembed(target_url).await;
        const USER_AGENTS: [&str; 3] = [
            "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)",
            "WhatsApp/2.24.6.77 A",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        ];

        for ua in USER_AGENTS {
            let Ok(mut response) = self.http_client.get_safe_with_ua(target_url, Some(ua)).await else {
                continue;
            };

            if !response.status().is_success() {
                continue;
            }

            if let Some(content_type) = response.headers().get(reqwest::header::CONTENT_TYPE) {
                let ct_str = content_type.to_str().unwrap_or_default().to_lowercase();
                if !ct_str.contains("text/html") && !ct_str.contains("application/xhtml") {
                    return Ok(ExtractedMetadata {
                        title: None,
                        description: Some(format!("Content-Type: {}", ct_str)),
                        image_url: None,
                        favicon_url: None,
                        site_name: None,
                    });
                }
            }

            let max_stream_bytes = 1024 * 1024;
            let mut buffer = Vec::with_capacity(64 * 1024);

            while let Ok(Some(chunk)) = response.chunk().await {
                buffer.extend_from_slice(&chunk);
                if buffer.len() >= max_stream_bytes {
                    break;
                }
                if let Ok(s) = std::str::from_utf8(&buffer[buffer.len().saturating_sub(1024)..]) {
                    if s.to_lowercase().contains("</head>") {
                        break;
                    }
                }
            }

            if let Ok(meta) = Self::parse_html(&buffer, target_url) {
                let meta = self.apply_oembed_and_reddit_fallbacks(meta, target_url, oembed.clone());
                if !Self::is_challenge_title(meta.title.as_deref()) && (meta.image_url.is_some() || ua == USER_AGENTS[2]) {
                    return Ok(meta);
                }
            }
        }

        // Browser fallback when Cloudflare / bot protection blocks plain HTTP
        let (html, final_url) = self.http_client.fetch_html_with_browser(target_url).await?;
        let meta = Self::parse_html(html.as_bytes(), &final_url)?;
        let meta = self.apply_oembed_and_reddit_fallbacks(meta, &final_url, oembed);
        if Self::is_challenge_title(meta.title.as_deref()) {
            return Err(AppError::BadRequest("Website blocked metadata extraction".to_string()));
        }
        Ok(meta)
    }
}
