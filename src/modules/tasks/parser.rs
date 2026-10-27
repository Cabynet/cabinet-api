use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFolder {
    pub name: String,
    pub parent_idx: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedBookmark {
    pub url: String,
    pub title: String,
    pub folder_idx: Option<usize>,
    pub add_date: Option<i64>,
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub struct NetscapeBookmarkDocument {
    pub folders: Vec<ParsedFolder>,
    pub bookmarks: Vec<ParsedBookmark>,
}

pub struct NetscapeParser;

impl NetscapeParser {
    pub fn parse(html: &str) -> NetscapeBookmarkDocument {
        let mut folders: Vec<ParsedFolder> = Vec::new();
        let mut bookmarks: Vec<ParsedBookmark> = Vec::new();

        let mut folder_stack: Vec<usize> = Vec::new();
        let mut pending_folder_idx: Option<usize> = None;

        let mut chars = html.char_indices().peekable();

        while let Some((i, c)) = chars.next() {
            if c != '<' {
                continue;
            }

            // Find closing '>'
            let rest = &html[i..];
            let tag_end_idx = match rest.find('>') {
                Some(idx) => idx,
                None => break,
            };

            let tag_content = &rest[1..tag_end_idx];

            let tag_lower = tag_content.trim().to_lowercase();
            let tag_name = tag_lower
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('/');

            let is_close_tag = tag_content.trim_start().starts_with('/');

            if is_close_tag {
                if tag_name == "dl" {
                    folder_stack.pop();
                }
            } else if tag_name == "dl" {
                if let Some(f_idx) = pending_folder_idx.take() {
                    folder_stack.push(f_idx);
                }
            } else if tag_name == "h3" {
                // Folder title
                let full_tag_end = i + tag_end_idx + 1;
                let remaining_html = &html[full_tag_end..];
                if let Some(h3_close) = remaining_html.to_lowercase().find("</h3>") {
                    let folder_name = decode_entities(&remaining_html[..h3_close])
                        .trim()
                        .to_string();

                    let parent_idx = folder_stack.last().copied();
                    let new_idx = folders.len();
                    folders.push(ParsedFolder {
                        name: if folder_name.is_empty() {
                            "Untitled Folder".to_string()
                        } else {
                            folder_name
                        },
                        parent_idx,
                    });
                    pending_folder_idx = Some(new_idx);
                }
            } else if tag_name == "a" {
                // Bookmark link: extract href and title
                let href = Self::extract_attribute(tag_content, "href");
                let add_date = Self::extract_attribute(tag_content, "add_date")
                    .and_then(|s| s.parse::<i64>().ok());

                if let Some(url_str) = href {
                    let trimmed_url = url_str.trim();
                    if Url::parse(trimmed_url).is_ok() {
                        let full_tag_end = i + tag_end_idx + 1;
                        let remaining_html = &html[full_tag_end..];
                        let title = if let Some(a_close) = remaining_html.to_lowercase().find("</a>") {
                            let text = &remaining_html[..a_close];
                            decode_entities(text).trim().to_string()
                        } else {
                            trimmed_url.to_string()
                        };

                        bookmarks.push(ParsedBookmark {
                            url: trimmed_url.to_string(),
                            title: if title.is_empty() {
                                trimmed_url.to_string()
                            } else {
                                title
                            },
                            folder_idx: folder_stack.last().copied(),
                            add_date,
                        });
                    }
                }
            }
        }

        NetscapeBookmarkDocument { folders, bookmarks }
    }

    fn extract_attribute(tag: &str, attr_name: &str) -> Option<String> {
        let tag_lower = tag.to_lowercase();
        let attr_pattern = format!("{}=", attr_name.to_lowercase());

        if let Some(pos) = tag_lower.find(&attr_pattern) {
            let after_equal = &tag[pos + attr_pattern.len()..].trim_start();
            if after_equal.starts_with('"') {
                let rest = &after_equal[1..];
                let end = rest.find('"')?;
                Some(decode_entities(&rest[..end]))
            } else if after_equal.starts_with('\'') {
                let rest = &after_equal[1..];
                let end = rest.find('\'')?;
                Some(decode_entities(&rest[..end]))
            } else {
                let end = after_equal
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(after_equal.len());
                Some(decode_entities(&after_equal[..end]))
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_netscape_html_with_nested_folders() {
        let sample = r#"
            <!DOCTYPE NETSCAPE-Bookmark-file-1>
            <TITLE>Bookmarks</TITLE>
            <H1>Bookmarks</H1>
            <DL><p>
                <DT><A HREF="https://root.example.com" ADD_DATE="1600000000">Root Bookmark</A>
                <DT><H3 ADD_DATE="1600000001">Development</H3>
                <DL><p>
                    <DT><A HREF="https://rust-lang.org">Rust Official</A>
                    <DT><H3>Web Frameworks</H3>
                    <DL><p>
                        <DT><A HREF="https://tokio.rs">Tokio &amp; Axum</A>
                    </DL><p>
                </DL><p>
            </DL><p>
        "#;

        let doc = NetscapeParser::parse(sample);

        assert_eq!(doc.folders.len(), 2);
        assert_eq!(doc.folders[0].name, "Development");
        assert_eq!(doc.folders[0].parent_idx, None);

        assert_eq!(doc.folders[1].name, "Web Frameworks");
        assert_eq!(doc.folders[1].parent_idx, Some(0)); // Child of Development!

        assert_eq!(doc.bookmarks.len(), 3);
        assert_eq!(doc.bookmarks[0].title, "Root Bookmark");
        assert_eq!(doc.bookmarks[0].folder_idx, None);
        assert_eq!(doc.bookmarks[0].add_date, Some(1600000000));

        assert_eq!(doc.bookmarks[1].title, "Rust Official");
        assert_eq!(doc.bookmarks[1].folder_idx, Some(0)); // In Development folder

        assert_eq!(doc.bookmarks[2].title, "Tokio & Axum"); // decoded entity
        assert_eq!(doc.bookmarks[2].folder_idx, Some(1)); // In Web Frameworks folder
    }
}
