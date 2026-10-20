//! `GET /models` against an OpenAI-compatible endpoint.
//!
//! The onboarding wizard needs two facts from a provider before it stores
//! anything: the endpoint accepts this key, and it can name the models it
//! serves. `/models` answers both in one call without spending a completion, and
//! it is part of the OpenAI-compatible surface every deployment 1.0 supports
//! implements (9router, Ollama, vLLM, OpenRouter).
//!
//! This lives in the provider layer, next to the adapter, so `eris-host` never
//! grows an HTTP client of its own and the failure taxonomy stays the six
//! [`LlmError`] variants. Nothing here admits a key value into an error, a log
//! line, or a stored row: the caller holds the [`SecretKey`] and this function
//! only ever puts it in one header.

use std::time::Duration;

use core_domain::{LlmError, ProviderProfile};
use reqwest::Client;

use std::collections::BTreeMap;

use super::{SecretKey, classify};

/// A slow gateway is a fact of life; a gateway that will not accept a TCP
/// connection is not. Same split as the chat adapter.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total budget for the model list. It is a metadata call, not a turn.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The model list one probe returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCatalog {
    /// Model ids, deduplicated and sorted, exactly as the provider spelled them.
    pub models: Vec<String>,
    /// The context window each entry declared, for the entries that declared
    /// one. Absent is absent: a gateway that does not publish a window does not
    /// get a guessed one, and the UI says "unknown" rather than inventing a
    /// percentage from a number nobody sent.
    pub context_lengths: BTreeMap<String, u64>,
}

impl ModelCatalog {
    /// The declared window for one model, when the provider published it.
    #[must_use]
    pub fn context_length(&self, model: &str) -> Option<u64> {
        self.context_lengths.get(model).copied()
    }
}

/// Ask a configured endpoint which models it serves.
pub async fn list_models(
    profile: &ProviderProfile,
    key: &SecretKey,
) -> Result<ModelCatalog, LlmError> {
    let url = format!("{}/models", profile.base_url);
    let http = Client::builder()
        .user_agent(concat!("kallisti-eris/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| classify::protocol(format!("could not build the HTTP client: {error}")))?;

    let response = http
        .get(&url)
        .bearer_auth(key.expose())
        .send()
        .await
        .map_err(|error| {
            classify::classify_request_failure(
                error.is_connect(),
                error.is_timeout(),
                error.is_body(),
                &format!("{url}: {error}"),
            )
        })?;

    let status = response.status();
    let retry_after_secs = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(classify::classify_http(
            status.as_u16(),
            &body,
            retry_after_secs,
        ));
    }

    let body = response.text().await.map_err(|error| {
        classify::classify_transport(
            classify::TransportFailure::Body,
            &format!("{url}: could not read the model list: {error}"),
        )
    })?;
    parse_model_list(&body).map_err(|detail| {
        classify::protocol(format!(
            "{url} answered 200 but the body is not a model list: {detail}"
        ))
    })
}

/// Parse the two shapes an OpenAI-compatible `/models` call returns: the
/// documented `{"data": [{"id": ...}]}` envelope and a bare array of ids.
pub fn parse_model_list(body: &str) -> Result<ModelCatalog, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("not JSON: {error}"))?;
    let entries = match &value {
        serde_json::Value::Object(object) => object
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "the object has no data array".to_owned())?,
        serde_json::Value::Array(entries) => entries,
        _ => return Err("the body is neither an object nor an array".to_owned()),
    };

    let mut models: Vec<String> = Vec::with_capacity(entries.len());
    let mut context_lengths: BTreeMap<String, u64> = BTreeMap::new();
    for entry in entries {
        let id = match entry {
            serde_json::Value::Object(object) => object
                .get("id")
                .or_else(|| object.get("model"))
                .and_then(serde_json::Value::as_str),
            serde_json::Value::String(text) => Some(text.as_str()),
            _ => None,
        };
        let Some(id) = id.map(str::trim) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        // Three spellings are in the wild for the same fact. Any positive one is
        // the window; anything else is treated as unreported.
        if let serde_json::Value::Object(object) = entry {
            let declared = ["context_length", "context_window", "max_context_tokens"]
                .iter()
                .filter_map(|key| object.get(*key).and_then(serde_json::Value::as_u64))
                .find(|value| *value > 0);
            if let Some(tokens) = declared {
                context_lengths.insert(id.to_owned(), tokens);
            }
        }
        models.push(id.to_owned());
    }
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("the list names no models".to_owned());
    }
    Ok(ModelCatalog {
        models,
        context_lengths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_envelope_parses() {
        let catalog = parse_model_list(
            r#"{"object":"list","data":[{"id":"b/model"},{"id":"a/model"},{"id":"a/model"}]}"#,
        )
        .expect("parses");
        assert_eq!(catalog.models, vec!["a/model", "b/model"]);
    }

    #[test]
    fn a_declared_context_window_is_kept_and_a_missing_one_stays_missing() {
        let catalog = parse_model_list(
            r#"{"data":[
                 {"id":"a/model","context_length":272000},
                 {"id":"b/model","context_window":128000},
                 {"id":"c/model","max_context_tokens":8000},
                 {"id":"d/model"},
                 {"id":"e/model","context_length":0}
               ]}"#,
        )
        .expect("parses");
        assert_eq!(catalog.context_length("a/model"), Some(272_000));
        assert_eq!(catalog.context_length("b/model"), Some(128_000));
        assert_eq!(catalog.context_length("c/model"), Some(8_000));
        assert_eq!(catalog.context_length("d/model"), None);
        assert_eq!(
            catalog.context_length("e/model"),
            None,
            "a zero is not a window"
        );
    }

    #[test]
    fn a_bare_array_of_ids_parses() {
        let catalog = parse_model_list(r#"["b", "a"]"#).expect("parses");
        assert_eq!(catalog.models, vec!["a", "b"]);
    }

    #[test]
    fn garbage_is_an_error_not_an_empty_catalog() {
        for body in ["not json", "{}", r#"{"data":[]}"#, r#"{"data":[{}]}"#, "42"] {
            assert!(
                parse_model_list(body).is_err(),
                "{body} must not become a catalog"
            );
        }
    }
}
