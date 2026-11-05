use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// API error: a status code and a message the UI can show.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn not_found(what: &str) -> Self {
        ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        let e = e.into();
        tracing::warn!("{e:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}
