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

use core_domain::{ApiDialect, LlmError, ProviderProfile};
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
    /// The request shape each entry declared it is served by.
    ///
    /// Only entries that stated one appear. A gateway whose catalogue carries no
    /// endpoint list says nothing here, which is the same as saying "the shape
    /// this profile already uses": a missing entry is never a guess.
    pub dialects: BTreeMap<String, ApiDialect>,
    /// The substrings that mark a model this catalog lists but the endpoint
    /// refused to serve, taken from a reachability pass. Order preserved.
    pub unreachable: Vec<String>,
}

impl ModelCatalog {
    /// The declared window for one model, when the provider published it.
    #[must_use]
    pub fn context_length(&self, model: &str) -> Option<u64> {
        self.context_lengths.get(model).copied()
    }

    /// The declared dialect for one model, when the provider published one.
    #[must_use]
    pub fn dialect(&self, model: &str) -> Option<ApiDialect> {
        self.dialects.get(model).copied()
    }

    /// The models this catalog lists that the endpoint refused, without their
    /// substring detail.
    #[must_use]
    pub fn unreachable_ids(&self) -> Vec<&str> {
        self.unreachable
            .iter()
            .filter_map(|line| line.split_once('\t').map(|(id, _)| id))
            .collect()
    }

    /// Record the models a reachability pass found dead or unavailable.
    ///
    /// The model ids stay in `models`: a catalogue that quietly dropped them
    /// would make a probe-clean provider look like one that never served them.
    /// What changes is that the caller can now tell the difference between
    /// "listed" and "listed and answers", which is the distinction the picker
    /// needs and the old shape could not express.
    pub fn mark_unreachable(&mut self, lines: Vec<String>) {
        self.unreachable = lines;
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

/// Render one unreachable entry: the model id, a tab, and why.
///
/// The reason is the provider's own words, already trimmed by the caller. The
/// tab is deliberate: a model id never contains one, so a reader can split the
/// two halves and a UI can show the id as a value rather than parsing prose.
#[must_use]
pub fn unreachable_line(model: &str, reason: &str) -> String {
    // A reason is provider prose and can carry anything, including the two
    // characters this encoding depends on: a newline would split one entry into
    // two rows, and a tab would make three halves out of two. Both become
    // spaces. The model id is passed through verbatim, because it has to match
    // the id the catalogue listed for the picker to link the two.
    let reason: String = reason
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    format!("{model}\t{}", reason.trim())
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
    let mut dialects: BTreeMap<String, ApiDialect> = BTreeMap::new();
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
            if let Some(dialect) = declared_dialect(object) {
                dialects.insert(id.to_owned(), dialect);
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
        dialects,
        unreachable: Vec::new(),
    })
}

/// The request shape a catalogue entry declared, when it declared one.
///
/// The precedence is deliberate. An entry that serves the OpenAI shape is read
/// that way even if it also serves Messages, because that is the shape every
/// deployment in 1.0 already sends to and the one this daemon has proved it can
/// build. Only an entry that refuses the OpenAI shape and names Messages is
/// read as Messages, which is exactly the Claude case: the endpoint says so
/// itself, so nothing here guesses.
///
/// An entry with no endpoint list, or with a list naming only shapes this build
/// cannot speak (`/responses`), yields `None`: the profile's dialect stands.
#[must_use]
pub fn declared_dialect(object: &serde_json::Map<String, serde_json::Value>) -> Option<ApiDialect> {
    let endpoints = object.get("supported_endpoints")?.as_array()?;
    let names: Vec<&str> = endpoints
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .collect();
    if names.contains(&"/chat/completions") {
        return Some(ApiDialect::ChatCompletions);
    }
    if names.contains(&"/messages") {
        return Some(ApiDialect::Messages);
    }
    None
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
    fn a_declared_endpoint_list_becomes_a_dialect() {
        let catalog = parse_model_list(
            r#"{"data":[
                 {"id":"claude-sonnet-5-5","supported_endpoints":["/messages"]},
                 {"id":"deepseek/deepseek-v4-flash",
                  "supported_endpoints":["/chat/completions","/responses"]},
                 {"id":"both","supported_endpoints":["/messages","/chat/completions"]},
                 {"id":"unlisted"}
               ]}"#,
        )
        .expect("parses");
        assert_eq!(
            catalog.dialect("claude-sonnet-5-5"),
            Some(ApiDialect::Messages)
        );
        assert_eq!(
            catalog.dialect("deepseek/deepseek-v4-flash"),
            Some(ApiDialect::ChatCompletions)
        );
        assert_eq!(
            catalog.dialect("both"),
            Some(ApiDialect::ChatCompletions),
            "an entry that serves the OpenAI shape is read that way"
        );
        assert_eq!(
            catalog.dialect("unlisted"),
            None,
            "an entry that declared nothing is not guessed at"
        );
        assert_eq!(catalog.models.len(), 4, "a declaration never drops a model");
    }

    #[test]
    fn an_entry_naming_only_a_shape_we_cannot_speak_declares_nothing() {
        let catalog =
            parse_model_list(r#"{"data":[{"id":"m/one","supported_endpoints":["/responses"]}]}"#)
                .expect("parses");
        assert_eq!(catalog.dialect("m/one"), None);
    }

    #[test]
    fn unreachable_models_are_recorded_without_leaving_the_catalogue() {
        let mut catalog =
            parse_model_list(r#"{"data":[{"id":"a/model"},{"id":"b/model"}]}"#).expect("parses");
        assert!(catalog.unreachable_ids().is_empty());
        catalog.mark_unreachable(vec![unreachable_line("b/model", "404 not found")]);
        assert_eq!(catalog.models.len(), 2, "a listed model stays listed");
        assert_eq!(catalog.unreachable_ids(), vec!["b/model"]);
        assert_eq!(
            catalog.unreachable,
            vec!["b/model\t404 not found".to_owned()],
            "the provider's own reason travels with the id"
        );
    }

    #[test]
    fn a_reason_cannot_smuggle_a_newline_or_a_tab_into_the_row() {
        let line = unreachable_line("m/one", "broken\tbadly\nand\r\nagain");
        assert_eq!(line, "m/one\tbroken badly and  again");
        assert_eq!(line.split('\t').count(), 2, "one tab, two halves");
        assert_eq!(
            line.lines().count(),
            1,
            "a reason with a newline must not become two rows"
        );

        // The id is untouched, so the row still names the model the catalogue
        // listed and a picker can link the two by exact equality.
        assert!(unreachable_line("weird/id", "x").starts_with("weird/id\t"));
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
