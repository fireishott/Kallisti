//! Speaking an answer.
//!
//! The phone could read an answer and never hear one. Vision already routes
//! through an auxiliary role, and speech is the same shape of job: a side channel
//! with its own endpoint and its own failure, not a different turn.
//!
//! Two halves, and both matter:
//!
//! - **The voice list comes from here.** A client that hardcodes voices shows
//!   voices the daemon cannot ask for, and a voice the daemon accepts never
//!   appears. The list is this module's, the setting is validated against it, and
//!   `GET /v1/voices` publishes it.
//! - **A refusal is named.** With no engine configured, `POST /v1/speak` answers
//!   `503 tts_not_configured` rather than silence, so the phone shows a state
//!   instead of a dead button.
//!
//! Synthesis goes to the provider's `/audio/speech`, the same base URL and key the
//! chat turns use. That is the whole reason this can ship without a local engine.
//!
//! A second engine exists beside that one and is off unless an operator stores it.
//! [`Engine::ChatAudio`] asks an OpenAI-compatible aggregation endpoint for audio
//! through the chat shapes it already serves: `/chat/completions` with an audio
//! modality for speech, and `/responses` with an audio input for transcription.
//! The default engine is [`Engine::AudioEndpoint`], so an install that never
//! writes `speech.engine` takes exactly the path it took before this row existed.

/// The `settings` row that arms speech.
pub const SETTING_ENABLED: &str = "tts.enabled";

/// The `settings` row holding the chosen voice.
pub const SETTING_VOICE: &str = "tts.voice";

/// The `settings` row overriding the speech endpoint.
pub const SETTING_BASE_URL: &str = "tts.base_url";

/// The `settings` row marking a keyless engine.
pub const SETTING_KEYLESS: &str = "tts.keyless";

/// The `settings` row choosing the speech engine.
pub const SETTING_ENGINE: &str = "speech.engine";

/// The `settings` row overriding the aggregation endpoint's base URL.
pub const SETTING_CHAT_BASE_URL: &str = "speech.chat_base_url";

/// The `settings` row holding the model the chat-audio engine runs.
pub const SETTING_CHAT_MODEL: &str = "speech.chat_model";

/// The voices the daemon accepts.
///
/// The local engine publishes exactly these. A name outside the list fails at
/// the engine with a message nobody can act on, so the list stays closed and
/// stays in one place.
pub const VOICES: [(&str, &str); 1] = [("amy", "Amy - neutral, en-US")];

/// The voice offered when the operator has not picked one.
pub const DEFAULT_VOICE: &str = "amy";

/// The most characters one request may carry.
///
/// A spoken answer is a summary, not a document: past this the provider call is
/// slow enough that the phone has already given up.
pub const MAX_SPEAK_CHARS: usize = 4_000;

/// The model name every OpenAI-shaped `/audio/speech` endpoint knows.
pub const MODEL: &str = "tts-1";

/// The default engine, and the only one an install gets without choosing.
pub const ENGINE_AUDIO_ENDPOINT: &str = "audio-endpoint";

/// The second engine: speech and transcription through the chat endpoint.
pub const ENGINE_CHAT_AUDIO: &str = "chat-audio";

/// The engines a `speech.engine` value may name, with the label the operator
/// reads.
pub const ENGINES: [(&str, &str); 2] = [
    ("audio-endpoint", "Audio endpoint - the classic /audio path"),
    (
        "chat-audio",
        "Chat audio - speech through the chat endpoint",
    ),
];

/// The sample rate the chat-audio engine speaks at, in hertz.
pub const CHAT_SAMPLE_RATE: u32 = 24_000;

/// One channel: the phone plays a voice, not a scene.
pub const CHAT_CHANNELS: u16 = 1;

/// Sixteen bits per sample, the width the chat engine returns PCM in.
pub const CHAT_BITS_PER_SAMPLE: u16 = 16;

/// The most characters a `speech.chat_model` value may hold.
///
/// Long enough for a provider-qualified id, short enough that a pasted
/// paragraph is refused before it becomes a row nobody can clear from the
/// picker.
pub const MAX_CHAT_MODEL_CHARS: usize = 200;

/// The model name to show when the chat-audio engine runs on the provider's own
/// model and no provider is configured to name it.
///
/// It is not a model id and is never sent anywhere: it is the honest answer to
/// "which model does this engine use" on a daemon that has not been pointed at
/// one yet.
pub const CHAT_MODEL_PROVIDER_DEFAULT: &str = "provider default";

/// Which engine answers a speech or transcription request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// `POST {base}/audio/speech` and `POST {base}/audio/transcriptions`.
    AudioEndpoint,
    /// `POST {base}/chat/completions` and `POST {base}/responses`.
    ChatAudio,
}

impl Engine {
    /// The value this engine stores in `speech.engine`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AudioEndpoint => ENGINE_AUDIO_ENDPOINT,
            Self::ChatAudio => ENGINE_CHAT_AUDIO,
        }
    }

    /// Read a stored value, or `None` when it names no engine.
    ///
    /// Comparison is case-insensitive and trimmed, the same rule the voice row
    /// uses: an operator who typed `Chat-Audio` meant the engine, not a typo.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        ENGINES
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(value))
            .map(|(key, _)| match *key {
                ENGINE_CHAT_AUDIO => Self::ChatAudio,
                _ => Self::AudioEndpoint,
            })
    }
}

/// The engine in force.
///
/// An unset row, an empty row, or an unknown value is the classic engine. A row
/// that was written by a newer build and then rolled back must not change the
/// wire path, so the fallback is the one that was always there.
#[must_use]
pub fn engine(db: &crate::db::Db) -> Engine {
    crate::web::store::settings_get(db, SETTING_ENGINE)
        .ok()
        .flatten()
        .as_deref()
        .and_then(Engine::parse)
        .unwrap_or(Engine::AudioEndpoint)
}

/// The speech model, which is an aux role because an audio model is not a chat
/// model and must not be held to the chat catalogue.
#[must_use]
pub fn model(db: &crate::db::Db) -> String {
    crate::web::store::settings_get(db, &crate::aux::setting_key("speech"))
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| MODEL.to_owned())
}

/// The model the chat-audio engine runs, when an operator pinned one.
#[must_use]
pub fn chat_model(db: &crate::db::Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_CHAT_MODEL)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The stored aggregation endpoint for the chat-audio engine, when one is set.
///
/// This is the row, not the endpoint in force. The engine falls back to the
/// configured provider's base URL when the row is absent, and a client editing
/// the row should see what is stored rather than what the fallback resolves to.
#[must_use]
pub fn chat_base_url(db: &crate::db::Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_CHAT_BASE_URL)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The model the chat-audio engine would actually ask for.
///
/// The call path tries `speech.chat_model` first and then the provider's model,
/// so a read has to try them in the same order: reporting only the stored row
/// described an engine as voiceless while it was speaking through the provider
/// model. When neither resolves - a daemon with no provider set up - the answer
/// names that state instead of handing a client an empty list.
#[must_use]
pub fn effective_chat_model(db: &crate::db::Db, provider_model: Option<&str>) -> String {
    chat_model(db)
        .or_else(|| {
            provider_model
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| CHAT_MODEL_PROVIDER_DEFAULT.to_owned())
}

/// The models `GET /v1/speech` should list for the engine in force.
///
/// The chat catalogue is a chat catalogue, not a voice catalogue, and only the
/// chat-audio engine is driven by a model name this daemon stores. So that
/// engine publishes the one model it would ask for - see
/// [`effective_chat_model`] - and the classic engine publishes none: its model is
/// already reported by `model`.
#[must_use]
pub fn voice_models(db: &crate::db::Db, provider_model: Option<&str>) -> Vec<String> {
    match engine(db) {
        Engine::ChatAudio => vec![effective_chat_model(db, provider_model)],
        Engine::AudioEndpoint => Vec::new(),
    }
}

/// Whether speech is switched on.
#[must_use]
pub fn enabled(db: &crate::db::Db) -> bool {
    matches!(
        crate::web::store::settings_get(db, SETTING_ENABLED)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("true")
    )
}

/// The stored voice, or the default when the row is empty or unknown.
#[must_use]
pub fn voice(db: &crate::db::Db) -> String {
    crate::web::store::settings_get(db, SETTING_VOICE)
        .ok()
        .flatten()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| VOICES.iter().any(|(key, _)| key == value))
        .unwrap_or_else(|| DEFAULT_VOICE.to_owned())
}

/// Whether a value names a voice.
#[must_use]
pub fn valid_voice(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    VOICES.iter().any(|(key, _)| *key == value)
}

/// Whether a value is an aggregation endpoint this daemon will call.
///
/// The engine builds `{base}/chat/completions` and `{base}/responses` from the
/// value, so anything that is not an absolute `http`/`https` URL with a host
/// would become a request to a path that does not exist. A store that accepted it
/// would look configured and fail on the first spoken word.
#[must_use]
pub fn valid_chat_base_url(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    reqwest::Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
        .unwrap_or(false)
}

/// One refusal, with the code the phone keys off.
///
/// The detail is the message the client reads when the static sentence for a code
/// would lose a fact the operator needs - the upstream status on
/// [`Refusal::ENGINE_FAILED`]. It never carries a credential; a refusal is a
/// sentence about what happened, not a dump of the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The HTTP status this daemon answers with.
    pub status: u16,
    /// The stable code a client keys off.
    pub code: &'static str,
    /// The message, when it says more than the code's fixed sentence.
    pub detail: Option<String>,
}

impl Refusal {
    /// The code for a non-2xx answer from the aggregation endpoint.
    pub const ENGINE_FAILED: &'static str = "speech_engine_failed";

    /// A refusal with the fixed sentence its code carries.
    #[must_use]
    pub const fn new(status: u16, code: &'static str) -> Self {
        Self {
            status,
            code,
            detail: None,
        }
    }

    /// A refusal that carries its own sentence.
    #[must_use]
    pub fn with_detail(status: u16, code: &'static str, detail: String) -> Self {
        Self {
            status,
            code,
            detail: Some(detail),
        }
    }
}

/// Speak one piece of text.
///
/// # Errors
///
/// A [`Refusal`]: `503` when speech is off or no engine is configured, `400` when
/// there is nothing to say, `502` when the provider refused.
pub async fn speak(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    text: &str,
) -> Result<(Vec<u8>, String), Refusal> {
    if !enabled(db) {
        return Err(Refusal::new(503, "tts_disabled"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Refusal::new(400, "tts_empty"));
    }
    let clipped: String = trimmed.chars().take(MAX_SPEAK_CHARS).collect();
    let chosen = voice(db);
    match engine(db) {
        Engine::AudioEndpoint => speak_audio_endpoint(db, config, &clipped, &chosen).await,
        Engine::ChatAudio => speak_chat_audio(db, config, &clipped, &chosen).await,
    }
}

/// The classic path: one JSON body, one audio answer.
async fn speak_audio_endpoint(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    text: &str,
    chosen: &str,
) -> Result<(Vec<u8>, String), Refusal> {
    // The endpoint is the engine's when a base URL is stored, and the chat
    // provider's otherwise. Only the second needs a provider: a keyless local
    // engine answers with no provider configured at all, which is the whole point
    // of the row.
    let base = match engine_base(db) {
        Some(base) => base,
        None => match config {
            Some(config) => config.base_url.trim_end_matches('/').to_owned(),
            None => return Err(Refusal::new(503, "tts_not_configured")),
        },
    };
    let endpoint = format!("{}/audio/speech", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model(db),
        "input": text,
        "voice": chosen,
    });
    let key = engine_key(db, config)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|_| Refusal::new(502, "tts_failed"))?;
    let mut request = client.post(&endpoint).json(&body);
    if let Some(key) = key.as_ref() {
        request = request.bearer_auth(key.as_str());
    }
    let response = request
        .send()
        .await
        .map_err(|_| Refusal::new(502, "tts_failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(upstream_refusal(status));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| Refusal::new(502, "tts_failed"))?
        .to_vec();
    if bytes.is_empty() {
        return Err(Refusal::new(502, "tts_failed"));
    }
    Ok((bytes, chosen.to_owned()))
}

/// The chat-audio path: PCM16 chunks over SSE, wrapped as a WAV for the phone.
async fn speak_chat_audio(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    text: &str,
    chosen: &str,
) -> Result<(Vec<u8>, String), Refusal> {
    let base = chat_base(db, config)?;
    let Some(model) = chat_model(db).or_else(|| config.map(|config| config.model.clone())) else {
        return Err(Refusal::new(503, "tts_not_configured"));
    };
    let key = engine_key(db, config)?;
    let endpoint = format!("{}/chat/completions", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": text}],
        "modalities": ["text", "audio"],
        "audio": {"voice": chosen, "format": "pcm16"},
        "stream": true,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|_| Refusal::new(502, "tts_failed"))?;
    let mut request = client
        .post(&endpoint)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&body);
    if let Some(key) = key.as_ref() {
        request = request.bearer_auth(key.as_str());
    }
    let response = request
        .send()
        .await
        .map_err(|_| Refusal::new(502, "tts_failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(upstream_refusal(status));
    }
    let stream = response
        .text()
        .await
        .map_err(|_| Refusal::new(502, "tts_failed"))?;
    let pcm = collect_pcm(&stream)?;
    if pcm.is_empty() {
        return Err(Refusal::new(502, "tts_failed"));
    }
    Ok((wav_from_pcm(&pcm), chosen.to_owned()))
}

/// The aggregation endpoint's base URL for the chat-audio engine.
///
/// `speech.chat_base_url` is an override; without it the configured provider's
/// base URL is the endpoint, because that is the endpoint the operator already
/// provisioned.
fn chat_base(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
) -> Result<String, Refusal> {
    chat_base_url(db)
        .or_else(|| config.map(|config| config.base_url.trim_end_matches('/').to_owned()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Refusal::new(503, "tts_not_configured"))
}

/// The key one request carries, or `None` for a keyless engine.
///
/// A keyless row is the only way to skip the key: the provider still has to be
/// configured, and a key that cannot be resolved is "not configured" rather than
/// a failed call with no explanation.
fn engine_key(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
) -> Result<Option<String>, Refusal> {
    let keyless = matches!(
        crate::web::store::settings_get(db, SETTING_KEYLESS)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("true")
    );
    if keyless {
        return Ok(None);
    }
    let Some(config) = config else {
        return Err(Refusal::new(503, "tts_not_configured"));
    };
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref)
        .map_err(|_| Refusal::new(503, "tts_not_configured"))?;
    Ok(Some(key.expose().to_owned()))
}

/// A non-2xx answer, named for what it is and carrying the upstream status.
///
/// The status is the only upstream fact in the message. The request body and the
/// key stay out of it: a refusal is read by a phone and by an operator, neither
/// of whom should learn a credential from an error page.
fn upstream_refusal(status: reqwest::StatusCode) -> Refusal {
    Refusal::with_detail(
        502,
        Refusal::ENGINE_FAILED,
        format!("the aggregation endpoint answered HTTP {status}"),
    )
}

/// Accumulate the base64 PCM chunks of one SSE answer.
fn collect_pcm(stream: &str) -> Result<Vec<u8>, Refusal> {
    use base64::Engine as _;
    let mut pcm = Vec::new();
    for line in stream.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            // SSE comments and event names are not audio.
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let event: serde_json::Value =
            serde_json::from_str(data).map_err(|_| Refusal::new(502, "tts_failed"))?;
        let Some(chunk) = event
            .pointer("/choices/0/delta/audio/data")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(chunk)
            .map_err(|_| Refusal::new(502, "tts_failed"))?;
        pcm.extend_from_slice(&bytes);
    }
    Ok(pcm)
}

/// Wrap PCM16 samples in the 44-byte WAV header the phone already reads.
#[must_use]
pub fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let block_align = CHAT_CHANNELS * (CHAT_BITS_PER_SAMPLE / 8);
    let byte_rate = CHAT_SAMPLE_RATE * u32::from(block_align);
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&36u32.saturating_add(data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&CHAT_CHANNELS.to_le_bytes());
    wav.extend_from_slice(&CHAT_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&CHAT_BITS_PER_SAMPLE.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// The engine base URL from `tts.base_url`, if one is stored.
pub fn engine_base(db: &crate::db::Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_BASE_URL)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Whether the configured engine can be asked anything.
///
/// The classic engine answers `/health`, so an unset or unreachable base reads
/// as "no engine" rather than "provider down" - which is the distinction the
/// readiness body draws for the operator.
///
/// The chat-audio engine has no such probe: it is the provider's own endpoint,
/// and a health route there would be a guess. Its readiness fact is the base it
/// resolves to, so a configured endpoint is reachable without spending a
/// request, and the first real answer is what reports the provider's health.
pub async fn engine_healthy(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
) -> bool {
    match engine(db) {
        Engine::ChatAudio => chat_base(db, config).is_ok(),
        Engine::AudioEndpoint => {
            let Some(base) = engine_base(db) else {
                return false;
            };
            let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            else {
                return false;
            };
            let endpoint = format!("{}/health", base.trim_end_matches('/'));
            matches!(
                client.get(&endpoint).send().await,
                Ok(response) if response.status().is_success()
            )
        }
    }
}

/// The largest clip `transcribe` accepts.
///
/// The engine refuses the same size. This cap exists so the daemon stops the
/// upload before the bytes are buffered, not after.
pub const MAX_TRANSCRIBE_BYTES: usize = 32 * 1024 * 1024;

/// Transcribe one clip.
///
/// # Errors
///
/// A [`Refusal`]: `503` when no engine is configured, `400` when the clip carries
/// no bytes, `413` past [`MAX_TRANSCRIBE_BYTES`], `502` when the engine refused.
pub async fn transcribe(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    audio: &[u8],
) -> Result<serde_json::Value, Refusal> {
    if audio.is_empty() {
        return Err(Refusal::new(400, "stt_empty"));
    }
    if audio.len() > MAX_TRANSCRIBE_BYTES {
        return Err(Refusal::new(413, "stt_too_large"));
    }
    match engine(db) {
        Engine::AudioEndpoint => transcribe_audio_endpoint(db, audio).await,
        Engine::ChatAudio => transcribe_chat_audio(db, config, audio).await,
    }
}

/// The classic path: the clip is the body, the answer is the engine's JSON.
async fn transcribe_audio_endpoint(
    db: &crate::db::Db,
    audio: &[u8],
) -> Result<serde_json::Value, Refusal> {
    let Some(base) = engine_base(db) else {
        return Err(Refusal::new(503, "stt_not_configured"));
    };
    let endpoint = format!("{}/audio/transcriptions", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|_| Refusal::new(502, "stt_failed"))?;
    let response = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "audio/wav")
        .body(audio.to_vec())
        .send()
        .await
        .map_err(|_| Refusal::new(502, "stt_failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(upstream_refusal(status));
    }
    response
        .json()
        .await
        .map_err(|_| Refusal::new(502, "stt_failed"))
}

/// The chat-audio path: one non-streamed response, one transcript.
async fn transcribe_chat_audio(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    audio: &[u8],
) -> Result<serde_json::Value, Refusal> {
    use base64::Engine as _;
    let base = chat_base(db, config).map_err(|_| Refusal::new(503, "stt_not_configured"))?;
    let Some(model) = chat_model(db).or_else(|| config.map(|config| config.model.clone())) else {
        return Err(Refusal::new(503, "stt_not_configured"));
    };
    let key = engine_key(db, config).map_err(|_| Refusal::new(503, "stt_not_configured"))?;
    let endpoint = format!("{}/responses", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": "Transcribe this audio verbatim. Answer with the transcript only."
                },
                {
                    "type": "input_audio",
                    "input_audio": {
                        "data": base64::engine::general_purpose::STANDARD.encode(audio),
                        "format": "wav",
                    }
                }
            ]
        }]
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|_| Refusal::new(502, "stt_failed"))?;
    let mut request = client.post(&endpoint).json(&body);
    if let Some(key) = key.as_ref() {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|_| Refusal::new(502, "stt_failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(upstream_refusal(status));
    }
    let answered: serde_json::Value = response
        .json()
        .await
        .map_err(|_| Refusal::new(502, "stt_failed"))?;
    let Some(text) = assistant_text(&answered) else {
        return Err(Refusal::new(502, "stt_failed"));
    };
    Ok(serde_json::json!({ "text": text }))
}

/// The assistant's text inside a `/responses` answer.
///
/// The message content is where a transcript lives; any other output item - a
/// reasoning block, a tool call - is not an answer to read back to a person.
fn assistant_text(body: &serde_json::Value) -> Option<String> {
    body.get("output")?
        .as_array()?
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .find_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;

    fn db() -> crate::db::Db {
        crate::db::Db::open_in_memory().expect("an in-memory database")
    }

    /// A one-request HTTP stub on loopback, the same shape the OAuth tests use.
    struct Stub {
        url: String,
        handle: Option<std::thread::JoinHandle<Vec<(String, String)>>>,
    }

    impl Stub {
        fn start(status: u16, content_type: &'static str, body: String) -> Self {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
            let address = listener.local_addr().expect("local address");
            let handle = std::thread::spawn(move || {
                let mut seen = Vec::new();
                if let Ok((stream, _)) = listener.accept()
                    && let Some(request) = serve_once(stream, status, content_type, &body)
                {
                    seen.push(request);
                }
                seen
            });
            Self {
                url: format!("http://{address}"),
                handle: Some(handle),
            }
        }

        fn requests(mut self) -> Vec<(String, String)> {
            match self.handle.take() {
                Some(handle) => handle.join().unwrap_or_default(),
                None => Vec::new(),
            }
        }
    }

    fn serve_once(
        mut stream: TcpStream,
        status: u16,
        content_type: &str,
        body: &str,
    ) -> Option<(String, String)> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok()?;

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_owned)
            {
                content_length = value.parse().unwrap_or(0);
            }
        }
        let mut body_bytes = vec![0u8; content_length];
        reader.read_exact(&mut body_bytes).ok()?;
        let request_body = String::from_utf8_lossy(&body_bytes).into_owned();

        let response = format!(
            "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            if status == 200 { "OK" } else { "Error" },
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).ok()?;
        stream.flush().ok()?;
        Some((request_line.trim_end().to_owned(), request_body))
    }

    fn sse(chunks: &[&[u8]]) -> String {
        use base64::Engine as _;
        let mut out = String::new();
        for chunk in chunks {
            let data = base64::engine::general_purpose::STANDARD.encode(chunk);
            out.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"audio\":{{\"data\":\"{data}\"}}}}}}]}}\n\n"
            ));
        }
        out.push_str("data: [DONE]\n\n");
        out
    }

    fn config(base_url: &str, model: &str) -> crate::web::chat::TurnConfig {
        crate::web::chat::TurnConfig {
            base_url: base_url.to_owned(),
            key_ref: core_domain::SecretRef::Env("KALLISTI_SPEECH_TEST_KEY".to_owned()),
            model: model.to_owned(),
            dialect: core_domain::ApiDialect::ChatCompletions,
            reasoning_split: false,
            direct: false,
            project: None,
        }
    }

    #[test]
    fn the_switch_is_off_until_stored_on() {
        let db = db();
        assert!(!enabled(&db), "speech is opt-in: it costs a provider call");
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        assert!(enabled(&db));
    }

    #[test]
    fn the_voice_falls_back_and_the_list_is_closed() {
        let db = db();
        assert_eq!(voice(&db), DEFAULT_VOICE);
        crate::web::store::settings_put(&db, SETTING_VOICE, "AMY").expect("store");
        assert_eq!(voice(&db), "amy", "case does not matter");
        crate::web::store::settings_put(&db, SETTING_VOICE, "pigeon").expect("store");
        assert_eq!(voice(&db), DEFAULT_VOICE, "an unknown voice is not shown");
        assert!(valid_voice("amy"));
        assert!(!valid_voice("pigeon"));
        assert!(!valid_voice("alloy"), "the engine has one voice, not six");
        assert_eq!(VOICES.len(), 1);
    }

    #[test]
    fn the_engine_is_the_audio_endpoint_until_it_is_stored() {
        let db = db();
        assert_eq!(
            engine(&db),
            Engine::AudioEndpoint,
            "an install that never writes the row keeps the classic path"
        );
        crate::web::store::settings_put(&db, SETTING_ENGINE, "CHAT-AUDIO").expect("store");
        assert_eq!(engine(&db), Engine::ChatAudio, "case does not matter");
        crate::web::store::settings_put(&db, SETTING_ENGINE, "pigeon").expect("store");
        assert_eq!(
            engine(&db),
            Engine::AudioEndpoint,
            "an unknown engine falls back rather than changing the wire path"
        );
        assert_eq!(Engine::parse("audio-endpoint"), Some(Engine::AudioEndpoint));
        assert_eq!(Engine::parse("chat-audio"), Some(Engine::ChatAudio));
        assert_eq!(Engine::parse("pigeon"), None);
        assert_eq!(ENGINES.len(), 2);
    }

    #[test]
    fn the_voice_models_list_names_the_effective_chat_model() {
        let db = db();
        assert!(
            voice_models(&db, None).is_empty(),
            "the classic engine lists none"
        );
        assert!(
            voice_models(&db, Some("provider-chat-model")).is_empty(),
            "the classic engine lists none even when a provider model resolves"
        );
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        assert_eq!(
            voice_models(&db, None),
            vec![CHAT_MODEL_PROVIDER_DEFAULT.to_owned()],
            "an unpinned model on a daemon with no provider still names the model it would fall \
             back to, rather than leaving a working engine voiceless"
        );
        assert_eq!(
            voice_models(&db, Some("  provider-chat-model  ")),
            vec!["provider-chat-model".to_owned()],
            "the provider's model is the effective model when none is pinned"
        );
        assert_eq!(
            voice_models(&db, Some("   ")),
            vec![CHAT_MODEL_PROVIDER_DEFAULT.to_owned()],
            "a blank provider model names no model"
        );
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "  chat-audio-model  ")
            .expect("store");
        assert_eq!(
            voice_models(&db, Some("provider-chat-model")),
            vec!["chat-audio-model".to_owned()],
            "a pinned model wins over the provider's"
        );
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_AUDIO_ENDPOINT).expect("store");
        assert!(
            voice_models(&db, Some("provider-chat-model")).is_empty(),
            "the classic engine still lists none"
        );
    }

    #[test]
    fn the_chat_rows_are_read_trimmed_and_absent_until_written() {
        let db = db();
        assert_eq!(chat_base_url(&db), None);
        assert_eq!(chat_model(&db), None);
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, "  https://lab.test/v1  ")
            .expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "  spoken-model  ")
            .expect("store");
        assert_eq!(chat_base_url(&db).as_deref(), Some("https://lab.test/v1"));
        assert_eq!(chat_model(&db).as_deref(), Some("spoken-model"));
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, "   ").expect("store");
        assert_eq!(
            chat_base_url(&db),
            None,
            "a blank row reads as unset, the same answer an absent one gives"
        );
    }

    #[test]
    fn a_chat_base_url_is_http_or_https_and_nothing_else() {
        for good in [
            "https://aggregation.test/v1",
            "http://127.0.0.1:20128/v1",
            "HTTPS://Aggregation.test/v1",
            "  https://aggregation.test  ",
        ] {
            assert!(valid_chat_base_url(good), "{good} is an endpoint");
        }
        for bad in [
            "ftp://aggregation.test",
            "aggregation.test",
            "ws://aggregation.test",
            "ws://127.0.0.1:20128/v1",
            "https://",
            "file:///etc/hosts",
            "   ",
        ] {
            assert!(!valid_chat_base_url(bad), "{bad} is not an http endpoint");
        }
        assert_eq!(MAX_CHAT_MODEL_CHARS, 200);
        assert_eq!(CHAT_MODEL_PROVIDER_DEFAULT, "provider default");
    }

    #[tokio::test]
    async fn speech_refuses_by_name_when_it_is_off_or_unconfigured() {
        let db = db();
        assert_eq!(
            speak(&db, None, "hi").await.unwrap_err(),
            Refusal::new(503, "tts_disabled")
        );
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        assert_eq!(
            speak(&db, None, "hi").await.unwrap_err(),
            Refusal::new(503, "tts_not_configured")
        );
        assert_eq!(
            speak(&db, None, "  ").await.unwrap_err(),
            Refusal::new(400, "tts_empty")
        );
    }

    #[tokio::test]
    async fn the_audio_endpoint_path_is_unchanged_when_the_engine_is_unset() {
        let db = db();
        let stub = Stub::start(200, "audio/wav", "RIFF-audio".to_owned());
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        crate::web::store::settings_put(&db, SETTING_BASE_URL, &stub.url).expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");

        let (bytes, voice) = speak(&db, None, "hello").await.expect("speech");
        assert_eq!(bytes, b"RIFF-audio");
        assert_eq!(voice, DEFAULT_VOICE);

        let requests = stub.requests();
        assert_eq!(requests[0].0, "POST /audio/speech HTTP/1.1");
        let sent: serde_json::Value = serde_json::from_str(&requests[0].1).expect("json");
        assert_eq!(sent["model"], MODEL);
        assert_eq!(sent["input"], "hello");
        assert_eq!(sent["voice"], DEFAULT_VOICE);
    }

    #[tokio::test]
    async fn the_audio_endpoint_transcribe_path_is_unchanged_when_the_engine_is_unset() {
        let db = db();
        let stub = Stub::start(200, "application/json", r#"{"text":"heard"}"#.to_owned());
        crate::web::store::settings_put(&db, SETTING_BASE_URL, &stub.url).expect("store");

        let heard = transcribe(&db, None, &[1, 2, 3]).await.expect("transcript");
        assert_eq!(heard["text"], "heard");

        let requests = stub.requests();
        assert_eq!(requests[0].0, "POST /audio/transcriptions HTTP/1.1");
    }

    #[tokio::test]
    async fn the_chat_engine_speaks_pcm_as_wav() {
        let pcm: &[u8] = &[0, 0, 1, 0, 2, 0, 3, 0];
        let db = db();
        let stub = Stub::start(200, "text/event-stream", sse(&[&pcm[..2], &pcm[2..]]));
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, &stub.url).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "chat-audio-model")
            .expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");

        let (wav, voice) = speak(&db, None, "hello").await.expect("speech");
        assert_eq!(voice, DEFAULT_VOICE);
        assert_eq!(wav.len(), 44 + pcm.len(), "one header and the samples");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            CHAT_SAMPLE_RATE
        );
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "one channel");
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "16-bit");
        assert_eq!(&wav[44..], pcm, "the samples are untouched");

        let requests = stub.requests();
        assert_eq!(requests[0].0, "POST /chat/completions HTTP/1.1");
        let sent: serde_json::Value = serde_json::from_str(&requests[0].1).expect("json");
        assert_eq!(sent["model"], "chat-audio-model");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["modalities"][0], "text");
        assert_eq!(sent["modalities"][1], "audio");
        assert_eq!(sent["audio"]["voice"], DEFAULT_VOICE);
        assert_eq!(sent["audio"]["format"], "pcm16");
        assert_eq!(sent["messages"][0]["role"], "user");
        assert_eq!(sent["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn the_chat_engine_falls_back_to_the_provider_base_and_model() {
        let db = db();
        let stub = Stub::start(200, "text/event-stream", sse(&[&[0u8, 0, 0, 0]]));
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");
        let config = config(&stub.url, "provider-chat-model");

        speak(&db, Some(&config), "hello").await.expect("speech");

        let requests = stub.requests();
        assert_eq!(requests[0].0, "POST /chat/completions HTTP/1.1");
        let sent: serde_json::Value = serde_json::from_str(&requests[0].1).expect("json");
        assert_eq!(
            sent["model"], "provider-chat-model",
            "an unpinned chat model follows the provider's model"
        );
    }

    #[tokio::test]
    async fn the_chat_engine_transcribes_through_the_responses_shape() {
        let db = db();
        let answer = r#"{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello there"}]}]}"#;
        let stub = Stub::start(200, "application/json", answer.to_owned());
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, &stub.url).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "chat-audio-model")
            .expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");

        let heard = transcribe(&db, None, &[1, 2, 3]).await.expect("transcript");
        assert_eq!(heard["text"], "hello there");

        let requests = stub.requests();
        assert_eq!(requests[0].0, "POST /responses HTTP/1.1");
        let sent: serde_json::Value = serde_json::from_str(&requests[0].1).expect("json");
        assert_eq!(sent["model"], "chat-audio-model");
        assert_eq!(sent["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(sent["input"][0]["content"][1]["type"], "input_audio");
        assert_eq!(
            sent["input"][0]["content"][1]["input_audio"]["format"],
            "wav"
        );
        use base64::Engine as _;
        assert_eq!(
            sent["input"][0]["content"][1]["input_audio"]["data"],
            base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3])
        );
    }

    #[tokio::test]
    async fn chat_audio_is_ready_when_its_base_resolves() {
        let db = db();
        assert!(
            !engine_healthy(&db, None).await,
            "the classic engine with no base is not ready"
        );
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        assert!(
            !engine_healthy(&db, None).await,
            "the chat engine with no base is not ready"
        );
        // A closed port is deliberate: the chat engine states its base rather
        // than spending a probe on the provider's endpoint.
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, "http://127.0.0.1:9")
            .expect("store");
        assert!(engine_healthy(&db, None).await);
    }

    #[tokio::test]
    async fn chat_audio_refuses_by_name_when_it_is_unconfigured() {
        let db = db();
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");
        assert_eq!(
            speak(&db, None, "hi").await.unwrap_err(),
            Refusal::new(503, "tts_not_configured")
        );
        assert_eq!(
            transcribe(&db, None, &[1]).await.unwrap_err(),
            Refusal::new(503, "stt_not_configured")
        );
    }

    #[tokio::test]
    async fn a_non_success_answer_names_the_engine_and_the_status() {
        let db = db();
        let stub = Stub::start(
            429,
            "application/json",
            r#"{"error":"slow down"}"#.to_owned(),
        );
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, &stub.url).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "chat-audio-model")
            .expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");

        let refusal = speak(&db, None, "hi").await.expect_err("refused");
        assert_eq!(refusal.status, 502);
        assert_eq!(refusal.code, Refusal::ENGINE_FAILED);
        let detail = refusal.detail.expect("a detail sentence");
        assert!(detail.contains("429"), "the status is named: {detail}");
        assert!(
            !detail.contains("slow down"),
            "the upstream body is not echoed: {detail}"
        );
    }

    #[tokio::test]
    async fn a_non_success_transcription_uses_the_same_engine_code() {
        let db = db();
        let stub = Stub::start(503, "application/json", "{}".to_owned());
        crate::web::store::settings_put(&db, SETTING_ENGINE, ENGINE_CHAT_AUDIO).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_BASE_URL, &stub.url).expect("store");
        crate::web::store::settings_put(&db, SETTING_CHAT_MODEL, "chat-audio-model")
            .expect("store");
        crate::web::store::settings_put(&db, SETTING_KEYLESS, "true").expect("store");

        let refusal = transcribe(&db, None, &[1]).await.expect_err("refused");
        assert_eq!(refusal.status, 502);
        assert_eq!(refusal.code, Refusal::ENGINE_FAILED);
        assert!(
            refusal
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("503")),
            "the upstream status is named"
        );
    }
}
