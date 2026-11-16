//! One envelope for daemon operations, whoever is calling.
//!
//! The trap this exists to close is two dialects of the same call: the phone's
//! HTTP surface, the dashboard's, and whatever the next client invents. A method
//! is named once here, dispatched once here, and answered in one shape.
//!
//! The envelope is context-keyed rather than path-keyed: the same `models.list`
//! answers a phone and an operator, and what differs is what the context is
//! allowed to see, not which code runs.

use serde_json::{Value, json};

use crate::device::SetupEngine;

/// Who is calling. The only thing that changes an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The device the bearer belongs to.
    pub device_id: String,
    /// Whether the bearer carries admin scope, which is what "operator" means
    /// here: a person at the host, not a phone.
    pub operator: bool,
}

impl Context {
    /// A short name for the context, for logs and for the reply.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        if self.operator { "operator" } else { "device" }
    }
}

/// Every method this shim serves, and the whole set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The model catalogue the host knows about.
    ModelsList,
    /// Whether the daemon is up and which version it is.
    Ping,
}

impl Method {
    /// The name on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelsList => "models.list",
            Self::Ping => "ping",
        }
    }

    /// Read a method name, or `None` when it is not one this shim serves.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "models.list" => Some(Self::ModelsList),
            "ping" => Some(Self::Ping),
            _ => None,
        }
    }

    /// Whether this method needs the operator context.
    #[must_use]
    pub const fn needs_operator(self) -> bool {
        matches!(self, Self::ModelsList)
    }
}

/// A refusal a caller can act on: an HTTP status, a stable code, a sentence.
#[derive(Debug)]
pub struct Refusal {
    /// HTTP status.
    pub status: u16,
    /// Stable code, so a client can branch without parsing prose.
    pub code: &'static str,
    /// What happened.
    pub message: String,
}

impl Refusal {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

/// Read the envelope a caller sent.
///
/// # Errors
///
/// A refusal naming what is missing or unknown, with the legal methods listed.
pub fn parse_envelope(body: &Value) -> Result<(Method, Value), Refusal> {
    let name = body
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::new(400, "method_missing", "the body carries no `method`"))?;
    let method = Method::parse(name).ok_or_else(|| {
        Refusal::new(
            404,
            "method_unknown",
            format!(
                "`{name}` is not a method. The methods are: models.list, ping"
            ),
        )
    })?;
    let params = body
        .get("params")
        .cloned()
        .unwrap_or(Value::Null);
    Ok((method, params))
}

/// Run one method and return its result.
///
/// # Errors
///
/// A refusal when the context is not allowed or the work failed.
pub fn dispatch(
    method: Method,
    _params: &Value,
    context: &Context,
    engine: &SetupEngine,
) -> Result<Value, Refusal> {
    if method.needs_operator() && !context.operator {
        return Err(Refusal::new(
            403,
            "scope_insufficient",
            format!("`{}` needs the operator context", method.as_str()),
        ));
    }
    match method {
        Method::Ping => Ok(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "context": context.kind(),
        })),
        Method::ModelsList => {
            crate::device::control::read_models_for_rpc(engine).map_err(|error| {
                Refusal::new(
                    503,
                    "models_unavailable",
                    format!("the model catalogue could not be read: {}", error.message()),
                )
            })
        }
    }
}

/// The envelope a success is wrapped in, so every method answers alike.
#[must_use]
pub fn success(method: Method, context: &Context, result: Value) -> Value {
    json!({
        "ok": true,
        "method": method.as_str(),
        "context": context.kind(),
        "result": result,
    })
}

/// The envelope a refusal is wrapped in, in the same shape.
#[must_use]
pub fn failure(refusal: &Refusal) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": refusal.code,
            "message": refusal.message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{Context, Method, Refusal, parse_envelope, success};

    #[test]
    fn a_method_is_named_once_and_read_once() {
        assert_eq!(Method::parse("models.list"), Some(Method::ModelsList));
        assert_eq!(Method::parse("ping"), Some(Method::Ping));
        assert_eq!(Method::parse("models.list "), Some(Method::ModelsList));
        assert_eq!(Method::parse("models.delete"), None);
    }

    #[test]
    fn an_unknown_method_is_refused_with_the_legal_set() {
        let refused = parse_envelope(&serde_json::json!({"method": "delete.everything"}))
            .expect_err("refused");
        assert_eq!(refused.status, 404);
        assert!(refused.message.contains("models.list"), "{}", refused.message);
    }

    #[test]
    fn a_missing_method_is_a_bad_request() {
        let refused = parse_envelope(&serde_json::json!({})).expect_err("refused");
        assert_eq!(refused.status, 400);
    }

    /// The success envelope names the method and the context, which is what makes
    /// one shape usable by both callers.
    #[test]
    fn a_success_states_the_method_and_the_context() {
        let context = Context {
            device_id: "phone".to_owned(),
            operator: false,
        };
        let body = success(Method::Ping, &context, serde_json::json!({"pong": true}));
        assert_eq!(body["method"], "ping");
        assert_eq!(body["context"], "device");
        assert_eq!(body["result"]["pong"], true);
    }

    #[test]
    fn a_refusal_keeps_its_code_and_message() {
        let refusal = Refusal {
            status: 403,
            code: "scope_insufficient",
            message: "no".to_owned(),
        };
        let body = super::failure(&refusal);
        assert_eq!(body["error"]["code"], "scope_insufficient");
    }
}
