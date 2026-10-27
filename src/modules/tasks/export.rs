use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::modules::folders::dto::FolderDto;

#[derive(Debug, Clone)]
pub struct BookmarkExportItem {
    pub id: Uuid,
    pub title: String,
    pub raw_url: Option<String>,
    pub folder_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn generate_netscape_html(
    folders: &[FolderDto],
    bookmarks: &[BookmarkExportItem],
) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE NETSCAPE-Bookmark-file-1>\n");
    out.push_str("<!-- This is an automatically generated file.\n     It will be read and overwritten.\n     DO NOT EDIT! -->\n");
    out.push_str("<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n");
    out.push_str("<TITLE>Bookmarks</TITLE>\n");
    out.push_str("<H1>Bookmarks</H1>\n");
    out.push_str("<DL><p>\n");

    // 1. Root bookmarks (folder_id is None)
    for bm in bookmarks.iter().filter(|b| b.folder_id.is_none()) {
        if let Some(ref url) = bm.raw_url {
            let add_date = bm.created_at.timestamp();
            out.push_str(&format!(
                "    <DT><A HREF=\"{}\" ADD_DATE=\"{}\">{}</A>\n",
                escape_html(url),
                add_date,
                escape_html(&bm.title)
            ));
        }
    }

    // 2. Folders hierarchy starting from root folders (parent_id is None)
    render_folders(None, folders, bookmarks, &mut out, 1);

    out.push_str("</DL><p>\n");
    out
}

fn render_folders(
    parent_id: Option<Uuid>,
    folders: &[FolderDto],
    bookmarks: &[BookmarkExportItem],
    out: &mut String,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let child_folders: Vec<&FolderDto> = folders.iter().filter(|f| f.parent_id == parent_id).collect();

    for folder in child_folders {
        let add_date = folder.created_at.timestamp();
        out.push_str(&format!(
            "{}<DT><H3 ADD_DATE=\"{}\">{}</H3>\n",
            indent,
            add_date,
            escape_html(&folder.name)
        ));
        out.push_str(&format!("{}<DL><p>\n", indent));

        // Bookmarks in this folder
        for bm in bookmarks.iter().filter(|b| b.folder_id == Some(folder.id)) {
            if let Some(ref url) = bm.raw_url {
                let bm_add_date = bm.created_at.timestamp();
                out.push_str(&format!(
                    "{}    <DT><A HREF=\"{}\" ADD_DATE=\"{}\">{}</A>\n",
                    indent,
                    escape_html(url),
                    bm_add_date,
                    escape_html(&bm.title)
                ));
            }
        }

        // Subfolders
        render_folders(Some(folder.id), folders, bookmarks, out, depth + 1);

        out.push_str(&format!("{}</DL><p>\n", indent));
    }
}
