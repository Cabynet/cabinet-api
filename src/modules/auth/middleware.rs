use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

pub async fn auth_middleware(req: Request, next: Next) -> Response {
    next.run(req).await
}
