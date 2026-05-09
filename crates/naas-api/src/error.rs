use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("requested {requested} bytes exceeds max of {max}")]
    RequestTooLarge { requested: usize, max: usize },

    #[error("requested zero bytes")]
    EmptyRequest,

    #[error("entropy pool closed")]
    PoolClosed,
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let status = match self {
            ServiceError::RequestTooLarge { .. } | ServiceError::EmptyRequest => {
                StatusCode::BAD_REQUEST
            }
            ServiceError::PoolClosed => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, self.to_string()).into_response()
    }
}
