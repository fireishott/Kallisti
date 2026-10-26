//! `eris probe` - one real turn through the configured provider.
//!
//! This is the smallest thing that proves the provider seam is not a diagram: a
//! configured endpoint, a resolved key reference, one streamed prompt, and the
//! usage the provider actually reported.
//!
//! Deliberate limits, because P4 is not P5: no listener, no database, no session
//! log, no turn machine. It prints the reply and the usage line and exits.
//!
//! It carries the persona too, because a probe that answered as a bare model
//! while the daemon answered as the assistant would be a probe of a different
//! product. The diagnostic line states the system prompt's length and never its
//! text.

use std::io::{self, Write};

use core_domain::{Capabilities, LlmError, ModelProfile, Provider, ProviderProfile};
use core_runtime::prompt::{PromptInputs, compile};
use core_runtime::provider::{ChatRequest, OpenAiCompatProvider, ProviderEvent, SecretKey};

use crate::config::KallistiConfig;
use crate::persona::PersonaConfig;

/// Everything that can make a probe fail.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The config file was unusable.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    /// The provider call failed. The six variants are the seam's whole surface.
    #[error(transparent)]
    Provider(#[from] LlmError),
    /// The persona file could not be used.
    #[error(transparent)]
    Persona(#[from] crate::persona::PersonaError),
    /// The persona did not fit in the system prompt.
    #[error(transparent)]
    Prompt(#[from] core_runtime::prompt::PromptError),
    /// Writing the reply failed.
    #[error("cannot write output: {0}")]
    Output(#[from] io::Error),
}

/// Build the adapter for a config, resolving the key reference against the
/// process environment.
pub fn provider_for(
    config: &KallistiConfig,
) -> Result<(OpenAiCompatProvider, SecretKey), ProbeError> {
    let profile = ProviderProfile::new(
        "openai-compat",
        config.base_url(),
        config.api_key_ref().clone(),
    )?;
    let model = ModelProfile::new(config.model());
    // An OpenAI-compatible gateway reports cache reads folded into its prompt
    // total. That is a property of the protocol, not of one vendor, so it is a
    // capability here rather than an if-let on a label.
    let capabilities = Capabilities::openai_compatible();
    let provider = OpenAiCompatProvider::new(profile, model, capabilities)?;
    // The daemon's one credential choke point, so a `--config` that names a
    // stored grant works in `eris probe` exactly as it does in a turn.
    let key = crate::oauth::credential::resolve_key_now(config.api_key_ref())?;
    Ok((provider, key))
}

/// Run one turn, streaming text to `out`.
pub async fn run(
    config: &KallistiConfig,
    persona: &PersonaConfig,
    prompt: &str,
    out: &mut impl Write,
    diag: &mut impl Write,
) -> Result<(), ProbeError> {
    let (provider, key) = provider_for(config)?;

    writeln!(
        diag,
        "eris probe: config={} POST {} model={} stream=on",
        config.path().display(),
        provider.chat_completions_url(),
        provider.model().id
    )?;

    // The identity, from the file, on this run: an edit is live without a
    // restart, which is the property this probe is also there to demonstrate.
    let persona_text = persona.load(None)?;
    let inputs = PromptInputs {
        persona: persona_text,
        ..PromptInputs::default()
    };
    let compiled = compile(&inputs)?;
    if compiled.is_empty() {
        writeln!(
            diag,
            "eris probe: no persona text is configured, so this turn carries no system message"
        )?;
    }

    let system = (!compiled.is_empty()).then(|| compiled.as_str().to_owned());
    let mut request = ChatRequest::with_system(system, prompt);
    request.max_output_tokens = config.max_output_tokens();

    // The body `stream_chat` is about to POST, inspected for its shape only.
    // This is the same `build_body` the request goes out through, so it is
    // evidence about the wire rather than a claim about our intent, and it
    // prints roles and a length - never the text.
    let body = provider.build_body(&request);
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let first_role = messages
        .first()
        .and_then(|message| message["role"].as_str())
        .unwrap_or("none");
    let system_bytes = messages
        .first()
        .filter(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
        .map(str::len)
        .unwrap_or(0);
    writeln!(
        diag,
        "eris probe: wire messages={} first_role={first_role} system_bytes={system_bytes}",
        messages.len()
    )?;

    let mut reasoning_bytes = 0usize;
    let completion = {
        let mut sink = |event: ProviderEvent| match event {
            ProviderEvent::Text(delta) => {
                // Written as it arrives: the reply is not buffered until the
                // stream ends, so a slow provider is visibly slow rather than
                // invisibly slow.
                let _ = out.write_all(delta.as_bytes());
                let _ = out.flush();
            }
            ProviderEvent::Reasoning(delta) => reasoning_bytes += delta.len(),
            ProviderEvent::ToolCall(_) => {}
        };
        provider.stream_chat(&request, &key, &mut sink).await?
    };

    // The reply has no trailing newline of its own.
    writeln!(out)?;
    writeln!(out, "{}", completion.usage)?;
    if let Some(reason) = completion.usage.cache_read().unavailable_reason() {
        writeln!(
            diag,
            "note: cache_read=provider_unavailable reason={} (the provider did not report cache reads; this is not zero)",
            reason.as_str()
        )?;
    }
    writeln!(
        diag,
        "finish_reason={} tool_calls={} reasoning_bytes={reasoning_bytes}",
        completion.finish_reason.as_deref().unwrap_or("none"),
        completion.tool_calls.len()
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config() -> KallistiConfig {
        KallistiConfig::parse(
            "KALLISTI_BASE_URL=http://127.0.0.1:20128/v1\n\
             KALLISTI_MODEL=ds/deepseek-v4-flash\n\
             KALLISTI_API_KEY_REF=env:KALLISTI_P4_TEST_KEY_ABSENT\n",
            &PathBuf::from("/tmp/eris.env"),
        )
        .expect("config should parse")
    }

    #[test]
    fn an_unresolvable_key_reference_fails_as_auth_before_any_request() {
        // The variable is named `_ABSENT` precisely so it does not exist: an
        // environment that happened to define it is not a supported state, so
        // the assertion can be strict instead of conditional.
        match provider_for(&config()) {
            Err(ProbeError::Provider(LlmError::Auth { detail })) => {
                assert!(
                    detail.contains("env:KALLISTI_P4_TEST_KEY_ABSENT"),
                    "{detail}"
                );
            }
            Ok(_) => panic!("a missing key variable must not resolve"),
            Err(other) => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn the_adapter_uses_the_configured_endpoint_and_model() {
        // Built directly, so no environment lookup happens.
        let profile = ProviderProfile::new(
            "openai-compat",
            config().base_url(),
            config().api_key_ref().clone(),
        )
        .expect("profile");
        let provider = OpenAiCompatProvider::new(
            profile,
            ModelProfile::new(config().model()),
            Capabilities::openai_compatible(),
        )
        .expect("provider");
        assert_eq!(
            provider.chat_completions_url(),
            "http://127.0.0.1:20128/v1/chat/completions"
        );
        assert_eq!(provider.model().id, "ds/deepseek-v4-flash");
    }
}
