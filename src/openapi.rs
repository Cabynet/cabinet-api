use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Cabinet API",
        version = "1.0.0",
        description = "High-performance modular monolith backend for Cabinet bookmark manager"
    ),
    components(
        schemas(
            crate::modules::auth::dto::AuthUser,
            crate::modules::auth::dto::RegisterRequest,
            crate::modules::auth::dto::LoginRequest,
            crate::modules::auth::dto::SessionResponse,
            crate::modules::auth::oauth::OAuthAuthorizeQuery,
            crate::modules::auth::oauth::OAuthUrlResponse,
            crate::modules::bookmarks::dto::CreateBookmarkRequest,
            crate::modules::bookmarks::dto::UpdateBookmarkRequest,
            crate::modules::bookmarks::dto::BookmarkListItem,
            crate::modules::bookmarks::dto::BookmarkListResponse,
            crate::modules::bookmarks::dto::BookmarkTagSummary,
            crate::modules::bookmarks::dto::BookmarkPreviewDto,
            crate::modules::bookmarks::dto::BookmarkPreviewsQuery,
            crate::modules::bookmarks::dto::BookmarkPreviewsResponse,
            crate::modules::bookmarks::dto::DuplicateItemDto,
            crate::modules::bookmarks::dto::DuplicateGroupDto,
            crate::modules::bookmarks::dto::DuplicateReportResponse,
            crate::modules::bookmarks::dto::DuplicateCleanupResponse,
            crate::modules::bookmarks::dto::LinkHealthCheckResponse,
            crate::modules::folders::dto::FolderDto,
            crate::modules::folders::dto::CreateFolderRequest,
            crate::modules::folders::dto::UpdateFolderRequest,
            crate::modules::folders::handlers::PublicFolderDto,
            crate::modules::folders::handlers::PublicBookmarkItem,
            crate::modules::folders::handlers::PublicShareResponse,
            crate::modules::tags::dto::TagDto,
            crate::modules::tags::dto::CreateTagRequest,
            crate::modules::notes::dto::UpdateNoteRequest,
            crate::modules::notes::dto::NoteResponse,
            crate::modules::notes::dto::NoteConflictError,
            crate::modules::media::dto::PresignUploadRequest,
            crate::modules::media::dto::PresignUploadResponse,
            crate::modules::media::dto::ConfirmUploadResponse,
            crate::modules::search::dto::SearchQuery,
            crate::modules::search::dto::SearchResultItem,
            crate::modules::search::dto::SearchResponse,
            crate::modules::tasks::dto::TaskStatusDto,
        )
    ),
    tags(
        (name = "auth", description = "Authentication & Sessions"),
        (name = "bookmarks", description = "Bookmarks management"),
        (name = "folders", description = "Folder hierarchy & Public sharing"),
        (name = "tags", description = "Tagging system"),
        (name = "notes", description = "Note editor autosave & conflict prevention"),
        (name = "media", description = "Cloudflare R2 Direct Uploads"),
        (name = "search", description = "PostgreSQL Full-Text & Trigram search"),
        (name = "tasks", description = "Background task tracking")
    )
)]
pub struct ApiDoc;

pub fn generate_openapi_json() -> String {
    ApiDoc::openapi().to_pretty_json().expect("Valid OpenAPI JSON")
}
