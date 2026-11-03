pub mod dto;
pub mod handlers;
pub mod middleware;
pub mod oauth;
pub mod service;

pub use dto::{AuthUser, LoginRequest, RegisterRequest, SessionResponse};
pub use handlers::router;
pub use middleware::{CurrentUser, OptionalCurrentUser, SessionToken};
pub use service::AuthService;

pub const SESSION_COOKIE_NAME: &str = "cabinet_session";
pub const SESSION_MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60; // 30 days
pub const OAUTH_STATE_TTL_SECS: u64 = 600; // 10 minutes

