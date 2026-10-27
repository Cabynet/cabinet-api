use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::modules::auth::middleware::CurrentUser;
use super::dto::{NoteConflictError, NoteResponse, UpdateNoteRequest};
use super::service::NotesService;

pub fn router() -> Router<PgPool> {
    Router::new().route("/:id", get(get_note_handler).put(update_note_handler))
}

#[utoipa::path(
    get,
    path = "/v1/bookmarks/{id}/note",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    responses(
        (status = 200, description = "Note document and plain text content", body = NoteResponse)
    ),
    tag = "notes"
)]
pub async fn get_note_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<NoteResponse>, AppError> {
    let service = NotesService::new(pool);
    let note = service.get_note(user.id, id).await?;
    Ok(Json(note))
}

#[utoipa::path(
    put,
    path = "/v1/bookmarks/{id}/note",
    params(
        ("id" = Uuid, Path, description = "Bookmark ID")
    ),
    request_body = UpdateNoteRequest,
    responses(
        (status = 200, description = "Note updated successfully", body = NoteResponse),
        (status = 409, description = "Conflict: note was modified by another session", body = NoteConflictError)
    ),
    tag = "notes"
)]
pub async fn update_note_handler(
    State(pool): State<PgPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateNoteRequest>,
) -> Result<Json<NoteResponse>, AppError> {
    let service = NotesService::new(pool);
    let note = service.update_note(user.id, id, payload).await?;
    Ok(Json(note))
}
