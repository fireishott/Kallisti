//! Announcing a finished turn on the phone that asked for it.
//!
//! Delivery lives in [`crate::apns`] and the registrations live in
//! [`crate::push`]. This module is the join: given a turn that just succeeded,
//! find every active registration for the device that owns it and send one
//! notification through the same cross-environment retry the operator's
//! `POST /v1/push/send` uses.
//!
//! Three rules the rest of the file exists to keep:
//!
//! 1. **A push never fails a turn.** The reply is already written and the phone
//!    can read it on next launch. Every error here is a log line, never a
//!    terminal frame.
//! 2. **Only a device turn is announced.** A browser turn has no phone waiting
//!    for it, and announcing the operator's own dashboard to his own phone is
//!    the notification spam this feature exists to avoid.
//! 3. **The retry is not repeated here.** The sender already carries the one
//!    cross-environment retry that matters; a second one on this path would
//!    hide a genuinely wrong token behind two rejections.

use std::sync::Arc;

use crate::apns::{ApnsSender, Outcome};
use crate::push::PushStore;

/// What a finished turn wants to say.
#[derive(Debug, Clone)]
pub struct Announcement {
    /// The device that ran the turn. Empty means nobody to tell.
    pub device_id: String,
    /// The job, so a tap addresses the turn rather than the conversation.
    pub job_id: String,
    /// The reply, already settled. Used for the body only.
    pub text: String,
    /// The conversation the turn wrote into, when it wrote into one.
    pub conversation: Option<String>,
    /// The assistant row, so a deep link lands on the message.
    pub message_id: Option<String>,
}

/// The outcome of one announcement, for the log line and for a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announced {
    /// Nothing to do: no sender, no store, no device, or the setting is off.
    Skipped(&'static str),
    /// The device has no active registration, so there is nothing to reach.
    NoRegistration,
    /// At least one registration was accepted by APNs.
    Delivered(usize),
    /// Every attempt failed, with Apple's or the network's own words.
    Failed(String),
}

impl Announced {
    /// Whether a registration was reached.
    #[must_use]
    pub fn delivered(&self) -> bool {
        matches!(self, Self::Delivered(count) if *count > 0)
    }
}

/// Whether a turn that ended in `status` is worth a notification.
///
/// Only `succeeded`. A cancelled turn is one the operator ended himself and a
/// failed one already said so on the stream he was reading, so a banner for
/// either is noise on the lock screen.
#[must_use]
pub fn worth_announcing(status: &str) -> bool {
    status == "succeeded"
}

/// Tell one device that its turn finished.
///
/// `enabled` comes from the operator's setting and is passed rather than read
/// here, so the decision to notify is visible at the call site.
pub async fn announce(
    sender: Option<Arc<ApnsSender>>,
    store: Option<&PushStore>,
    announcement: &Announcement,
    enabled: bool,
) -> Announced {
    if !enabled {
        return Announced::Skipped("push.notify_on_reply is off");
    }
    let Some(sender) = sender else {
        return Announced::Skipped("no APNs sender is configured");
    };
    let Some(store) = store else {
        return Announced::Skipped("the push store did not open");
    };
    if announcement.device_id.trim().is_empty() {
        return Announced::Skipped("the turn belongs to no device");
    }
    let targets = match store.active_registrations(&announcement.device_id) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%error, "cannot read the push registrations for a finished turn");
            return Announced::Failed(format!("cannot read the registrations: {error}"));
        }
    };
    if targets.is_empty() {
        return Announced::NoRegistration;
    }

    let (title, body, user_info) = crate::push::reply_notification(
        &announcement.text,
        announcement.conversation.as_deref(),
        &announcement.job_id,
        announcement.message_id.as_deref(),
    );

    let mut delivered = 0usize;
    let mut last_failure: Option<String> = None;
    for (apns_token, environment, bundle) in targets {
        let Some(apns_token) = apns_token.filter(|token| !token.trim().is_empty()) else {
            // A `relay` transport registration carries no token this host can
            // use. Nothing to retry and nothing to retire.
            continue;
        };
        let recorded_host = sender.host_for(Some(environment.as_str()));
        let outcome = sender
            .send_alert(
                &apns_token,
                bundle.as_deref(),
                Some(environment.as_str()),
                &title,
                &body,
                Some(crate::push::MESSAGE_READY_CATEGORY),
                Some(&user_info),
            )
            .await;

        // The same correction the send route makes: a token is valid against
        // one host, and an app that reports the wrong environment would
        // otherwise be rejected on every single turn.
        let (outcome, host) = if outcome.retires_token() {
            let other = sender.other_host(recorded_host);
            let retried = sender
                .send_alert_to(
                    other,
                    &apns_token,
                    bundle.as_deref(),
                    &title,
                    &body,
                    Some(crate::push::MESSAGE_READY_CATEGORY),
                    Some(&user_info),
                )
                .await;
            (retried, other)
        } else {
            (outcome, recorded_host)
        };

        match &outcome {
            Outcome::Sent => {
                delivered += 1;
                if host != recorded_host {
                    let proved = if host == crate::apns::PRODUCTION_HOST {
                        "production"
                    } else {
                        "development"
                    };
                    let _ = store.set_environment(&announcement.device_id, &apns_token, proved);
                    tracing::info!(
                        device_id = %announcement.device_id,
                        environment = %proved,
                        "corrected the recorded APNs environment from a delivered reply notice"
                    );
                }
            }
            Outcome::TokenInvalid => {
                // Dead token: retire it so the next turn does not pay for the
                // same rejection. A phone that reinstalls registers a new one.
                let _ = store.deactivate_token(&announcement.device_id, &apns_token);
                tracing::info!(
                    device_id = %announcement.device_id,
                    "APNs retired a push token; the registration is now inactive"
                );
                last_failure = Some("APNs retired the token".to_owned());
            }
            Outcome::Rejected { status, reason } => {
                last_failure = Some(format!("APNs rejected it: {status} {reason}"));
            }
            Outcome::Transient { detail } => {
                last_failure = Some(detail.clone());
            }
        }
    }

    if delivered > 0 {
        Announced::Delivered(delivered)
    } else {
        Announced::Failed(
            last_failure.unwrap_or_else(|| "no registration carried a token".to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> PushStore {
        PushStore::open_in_memory().expect("in-memory push store opens")
    }

    fn sender() -> Arc<ApnsSender> {
        Arc::new(
            ApnsSender::new(
                &crate::apns::generated_test_key_pem(),
                "M375486F3F",
                "58U7UPFS53",
                Some("net.fihonline.kallisti".to_owned()),
                "production",
            )
            .expect("the generated key builds a sender"),
        )
    }

    fn announcement() -> Announcement {
        Announcement {
            device_id: "device-1".to_owned(),
            job_id: "job-1".to_owned(),
            text: "The answer is 42.".to_owned(),
            conversation: Some("conv-1".to_owned()),
            message_id: Some("msg-1".to_owned()),
        }
    }

    fn register(store: &PushStore, token: &str) {
        store
            .upsert(
                "device-1",
                "direct",
                Some(token),
                "production",
                "net.fihonline.kallisti",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("register");
    }

    #[tokio::test]
    async fn no_sender_means_nothing_is_attempted() {
        let store = store();
        let outcome = announce(None, Some(&store), &announcement(), true).await;
        assert_eq!(outcome, Announced::Skipped("no APNs sender is configured"));
        assert!(!outcome.delivered());
    }

    #[tokio::test]
    async fn the_setting_off_wins_over_everything_else() {
        let store = store();
        let outcome = announce(sender().into(), Some(&store), &announcement(), false).await;
        assert_eq!(outcome, Announced::Skipped("push.notify_on_reply is off"));
    }

    #[tokio::test]
    async fn a_turn_owned_by_no_device_is_not_announced() {
        let store = store();
        let mut announcement = announcement();
        announcement.device_id.clear();
        let outcome = announce(sender().into(), Some(&store), &announcement, true).await;
        assert_eq!(outcome, Announced::Skipped("the turn belongs to no device"));
    }

    #[tokio::test]
    async fn a_device_with_no_active_registration_is_reported_as_such() {
        let store = store();
        let outcome = announce(sender().into(), Some(&store), &announcement(), true).await;
        assert_eq!(outcome, Announced::NoRegistration);
    }

    #[tokio::test]
    async fn a_retired_registration_is_not_attempted() {
        let store = store();
        register(&store, "token-1");
        assert_eq!(store.deactivate("device-1").expect("deactivate"), 1);
        let outcome = announce(sender().into(), Some(&store), &announcement(), true).await;
        assert_eq!(outcome, Announced::NoRegistration);
    }

    #[tokio::test]
    async fn a_registration_that_carries_no_token_is_not_an_attempt() {
        let store = store();
        // A relay registration: active, but with no APNs token to use.
        store
            .upsert(
                "device-1",
                "relay",
                None,
                "production",
                "net.fihonline.kallisti",
                Some("handle"),
                Some("grant"),
                Some("relay-id"),
                Some("relay-key"),
                None,
            )
            .expect("register");
        let outcome = announce(sender().into(), Some(&store), &announcement(), true).await;
        assert_eq!(
            outcome,
            Announced::Failed("no registration carried a token".to_owned())
        );
    }

    #[test]
    fn the_body_strips_reasoning_and_stays_short() {
        let long = format!(
            "<think>secret reasoning</think>{}{}",
            "word ".repeat(80),
            "end"
        );
        let (title, body, info) =
            crate::push::reply_notification(&long, Some("conv-1"), "job-1", Some("msg-1"));
        assert_eq!(title, "Kallisti");
        assert!(
            !body.contains("secret"),
            "reasoning never reaches the banner"
        );
        assert!(!body.contains('<'));
        assert!(body.ends_with("..."));
        // The cut lands on a word boundary, never mid-word.
        assert!(!body.contains("wor..."));
        assert_eq!(info["conversationId"], "conv-1");
        assert_eq!(info["jobId"], "job-1");
        assert_eq!(info["messageId"], "msg-1");
    }

    #[test]
    fn a_reply_that_is_only_reasoning_gets_a_placeholder_body() {
        let (_, body, _) =
            crate::push::reply_notification("<think>only thinking</think>", None, "job-1", None);
        assert_eq!(body, "Your reply is ready.");
    }

    #[test]
    fn a_short_reply_is_sent_whole_without_an_ellipsis() {
        let (_, body, _) =
            crate::push::reply_notification("Got it, all done.", Some("c"), "j", None);
        assert_eq!(body, "Got it, all done.");
    }

    #[test]
    fn only_a_succeeded_turn_is_worth_a_notification() {
        assert!(worth_announcing("succeeded"));
        assert!(!worth_announcing("cancelled"));
        assert!(!worth_announcing("failed"));
        assert!(!worth_announcing("running"));
        assert!(!worth_announcing(""));
    }

    #[test]
    fn an_absent_setting_means_on_and_only_an_explicit_no_means_off() {
        assert!(crate::push::notify_on_reply_enabled(None));
        assert!(crate::push::notify_on_reply_enabled(Some("")));
        assert!(crate::push::notify_on_reply_enabled(Some("true")));
        assert!(crate::push::notify_on_reply_enabled(Some("yes")));
        assert!(!crate::push::notify_on_reply_enabled(Some("false")));
        assert!(!crate::push::notify_on_reply_enabled(Some("OFF")));
    }
}
