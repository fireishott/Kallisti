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

/// The `settings` row that arms speech.
pub const SETTING_ENABLED: &str = "tts.enabled";

/// The `settings` row holding the chosen voice.
pub const SETTING_VOICE: &str = "tts.voice";

/// The `settings` row overriding the speech endpoint.
pub const SETTING_BASE_URL: &str = "tts.base_url";

/// The `settings` row marking a keyless engine.
pub const SETTING_KEYLESS: &str = "tts.keyless";

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

/// One refusal, with the code the phone keys off.
pub type Refusal = (u16, &'static str);

/// Speak one piece of text.
///
/// # Errors
///
/// A status and a code: `503` when speech is off or no engine is configured,
/// `400` when there is nothing to say, `502` when the provider refused.
pub async fn speak(
    db: &crate::db::Db,
    config: Option<&crate::web::chat::TurnConfig>,
    text: &str,
) -> Result<(Vec<u8>, String), Refusal> {
    if !enabled(db) {
        return Err((503, "tts_disabled"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err((400, "tts_empty"));
    }
    let Some(config) = config else {
        return Err((503, "tts_not_configured"));
    };
    let clipped: String = trimmed.chars().take(MAX_SPEAK_CHARS).collect();
    let chosen = voice(db);
    let base = crate::web::store::settings_get(db, SETTING_BASE_URL)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.base_url.trim_end_matches('/').to_owned());
    let endpoint = format!("{}/audio/speech", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": MODEL,
        "input": clipped,
        "voice": chosen,
    });
    let keyless = matches!(
        crate::web::store::settings_get(db, SETTING_KEYLESS)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("true")
    );
    let key = if keyless {
        None
    } else {
        Some(
            crate::oauth::credential::resolve_key_now(&config.key_ref)
                .map_err(|_| (503, "tts_not_configured"))?,
        )
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|_| (502, "tts_failed"))?;
    let mut request = client.post(&endpoint).json(&body);
    if let Some(key) = key.as_ref() {
        request = request.bearer_auth(key.expose());
    }
    let response = request.send().await.map_err(|_| (502, "tts_failed"))?;
    if !response.status().is_success() {
        return Err((502, "tts_failed"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| (502, "tts_failed"))?
        .to_vec();
    if bytes.is_empty() {
        return Err((502, "tts_failed"));
    }
    Ok((bytes, chosen))
}

/// The engine base URL from `tts.base_url`, if one is stored.
fn engine_base(db: &crate::db::Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_BASE_URL)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Whether the configured engine answers `/health`.
///
/// Only a keyless local engine has a `/health`, so an unset or unreachable base
/// reads as "no engine" rather than "provider down" - which is the distinction
/// the readiness body draws for the operator.
pub async fn engine_healthy(db: &crate::db::Db) -> bool {
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
    matches!(client.get(&endpoint).send().await, Ok(response) if response.status().is_success())
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
/// A status and a code: `503` when no engine base URL is stored, `400` when the
/// clip carries no bytes, `413` past [`MAX_TRANSCRIBE_BYTES`], `502` when the
/// engine refused.
pub async fn transcribe(db: &crate::db::Db, audio: &[u8]) -> Result<serde_json::Value, Refusal> {
    if audio.is_empty() {
        return Err((400, "stt_empty"));
    }
    if audio.len() > MAX_TRANSCRIBE_BYTES {
        return Err((413, "stt_too_large"));
    }
    let Some(base) = engine_base(db) else {
        return Err((503, "stt_not_configured"));
    };
    let endpoint = format!("{}/audio/transcriptions", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|_| (502, "stt_failed"))?;
    let response = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "audio/wav")
        .body(audio.to_vec())
        .send()
        .await
        .map_err(|_| (502, "stt_failed"))?;
    if !response.status().is_success() {
        return Err((502, "stt_failed"));
    }
    response.json().await.map_err(|_| (502, "stt_failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> crate::db::Db {
        crate::db::Db::open_in_memory().expect("an in-memory database")
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

    #[tokio::test]
    async fn speech_refuses_by_name_when_it_is_off_or_unconfigured() {
        let db = db();
        assert_eq!(
            speak(&db, None, "hi").await.unwrap_err(),
            (503, "tts_disabled")
        );
        crate::web::store::settings_put(&db, SETTING_ENABLED, "true").expect("store");
        assert_eq!(
            speak(&db, None, "hi").await.unwrap_err(),
            (503, "tts_not_configured")
        );
        assert_eq!(
            speak(&db, None, "  ").await.unwrap_err(),
            (400, "tts_empty")
        );
    }
}
