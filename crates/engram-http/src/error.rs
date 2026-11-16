//! Error responses.

use crate::*;

pub enum AppError {
    Core(Error),
    Serde(serde_json::Error),
    NotFound,
}

impl From<Error> for AppError {
    fn from(e: Error) -> Self {
        AppError::Core(e)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Serde(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::Core(Error::NotFound(s)) => (StatusCode::NOT_FOUND, s),
            AppError::Core(e @ (Error::Parse { .. } | Error::Project(_) | Error::Config(_))) => {
                (StatusCode::BAD_REQUEST, e.to_string())
            }
            AppError::Core(e @ Error::Pinned(_)) => (StatusCode::CONFLICT, e.to_string()),
            AppError::Core(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::Serde(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}
