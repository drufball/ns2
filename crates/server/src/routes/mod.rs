pub mod events_route;
pub mod hook;
pub mod issue;
pub mod session;
pub mod webhook;

use axum::{http::StatusCode, response::IntoResponse, Json};

/// Server-level error type. Converted directly into HTTP responses.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db error: {0}")]
    Db(#[from] db::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<issues::Error> for Error {
    fn from(e: issues::Error) -> Self {
        match e {
            issues::Error::Db(db_err) => Self::Db(db_err),
            issues::Error::Backend(issue_backend::Error::NotFound) => Self::NotFound,
            issues::Error::Backend(other) => {
                Self::BadRequest(other.to_string())
            }
            issues::Error::BadRequest(msg) => Self::BadRequest(msg),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            Self::NotFound | Self::Db(db::Error::NotFound) => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
