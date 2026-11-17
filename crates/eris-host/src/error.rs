//! The one error shape every response uses.
//!
//! A client never has to guess: an error is always
//! `{"code": "...", "message": "...", "request_id": "..."}` with an HTTP status
//! that matches, and the same `request_id` is on the `x-request-id` header.
//!
//! The request id is minted by [`assign_request_id`] before routing, so every
//! response carries one - including the 404 and 405 that never reach a handler.
//! That middleware is the reason a handler can demand `Extension<RequestId>`
//! without an `Option`.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use uuid::Uuid;

/// Header carrying the request id, on success and on failure.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// The per-request identifier minted by the middleware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    /// A fresh time-ordered id, so a log can be sorted by it.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

/// The stable JSON body of every error response. Field order is the wire order.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Stable machine-readable code. Clients branch on this, not on `message`.
    pub code: String,
    /// Human-readable detail. Never contains a secret or a key value.
    pub message: String,
    /// The id this request was assigned.
    pub request_id: String,
}

/// A typed error: an HTTP status plus the [`ErrorBody`] that goes on the wire.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    /// Build one explicitly.
    #[must_use]
    pub fn new(
        status: StatusCode,
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.into(),
                message: message.into(),
                request_id: request_id.into(),
            },
        }
    }

    /// 404 for a path the router does not serve.
    #[must_use]
    pub fn not_found(request_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message, request_id)
    }

    /// 405 for a path that exists with a different method.
    #[must_use]
    pub fn method_not_allowed(request_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            message,
            request_id,
        )
    }

    /// 500 for a bug or an unexpected failure.
    #[must_use]
    pub fn internal(request_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            message,
            request_id,
        )
    }

    /// 503 when the database cannot answer.
    #[must_use]
    pub fn db_unavailable(request_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "db_unavailable",
            message,
            request_id,
        )
    }

    /// The status this error will answer with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable code this error will answer with.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.body.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = self.body.request_id.clone();
        let mut response = (self.status, axum::Json(self.body)).into_response();
        if let Ok(value) = HeaderValue::from_str(&request_id) {
            response
                .headers_mut()
                .insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
        }
        response
    }
}

/// Mint a request id, hand it to the handlers, and echo it on the response.
///
/// Installed with `Router::layer`, so it wraps the fallback and the 405 handler
/// too: no response leaves the process without an id.
pub async fn assign_request_id(mut request: Request, next: Next) -> Response {
    let id = RequestId::generate();
    request.extensions_mut().insert(id.clone());
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&id.0) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_body_has_exactly_three_fields_in_a_stable_order() {
        let error = ApiError::not_found("req-1", "no route for /nope");
        let body = serde_json::to_string(&error.body).expect("serialisable");
        assert_eq!(
            body,
            r#"{"code":"not_found","message":"no route for /nope","request_id":"req-1"}"#
        );
    }

    #[test]
    fn each_constructor_sets_its_own_status_and_code() {
        assert_eq!(
            ApiError::not_found("r", "m").status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(ApiError::not_found("r", "m").code(), "not_found");
        assert_eq!(
            ApiError::method_not_allowed("r", "m").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            ApiError::internal("r", "m").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::db_unavailable("r", "m").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn request_ids_are_unique() {
        let a = RequestId::generate();
        let b = RequestId::generate();
        assert_ne!(a, b);
        assert_eq!(a.0.len(), 36, "a canonical uuid string");
    }
}
