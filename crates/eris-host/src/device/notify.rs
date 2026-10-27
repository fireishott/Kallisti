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
use crate::push::{NotifyScope, PushStore};

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
    /// Who this announcement reaches. `Origin` is the device above and is
    /// today's behaviour; `All` and `Targeted` reach past it.
    pub scope: NotifyScope,
    /// The device ids a [`NotifyScope::Targeted`] announcement names. Ignored
    /// under the other two scopes.
    pub devices: Vec<String>,
}

impl Announcement {
    /// An announcement that reaches only the device that ran the turn.
    ///
    /// The shape every caller had before scopes existed, so a call site that
    /// does not care about routing says so by not mentioning it.
    #[must_use]
    pub fn to_origin(device_id: String, job_id: String, text: String) -> Self {
        Self {
            device_id,
            job_id,
            text,
            conversation: None,
            message_id: None,
            scope: NotifyScope::Origin,
            devices: Vec::new(),
        }
    }
}

/// One registration an announcement will attempt, with the device it belongs
/// to.
///
/// The device id is carried per target rather than read from the announcement,
/// because under `all` and `targeted` the device that owns the turn is not
/// necessarily the device being reached, and a token retired or a corrected
/// environment has to be written against the row that carries it.
type Target = (String, Option<String>, String, Option<String>);

/// Who an announcement resolves to, before anything is sent.
enum Reachable {
    /// These registrations, in the order they will be attempted.
    Targets(Vec<Target>),
    /// Nobody, with the sentence that says why.
    Nobody(&'static str),
}

/// Turn a scope and a device list into the registrations to attempt.
///
/// Pure routing: no network, no delivery, so the decision can be tested on its
/// own. An empty result after a non-empty scope is [`Announced::NoRegistration`]
/// at the call site, which is a phone that has not registered.
fn reachable(
    store: &PushStore,
    announcement: &Announcement,
    live: Option<&[String]>,
) -> Result<Reachable, String> {
    match announcement.scope {
        NotifyScope::Origin => {
            if announcement.device_id.trim().is_empty() {
                // A browser turn has no owning device, so `origin` has nothing
                // to resolve to. Said plainly rather than falling through to a
                // broadcast this scope did not ask for.
                return Ok(Reachable::Nobody(
                    "the notify scope is origin and the turn belongs to no device",
                ));
            }
            let rows = store
                .active_registrations(&announcement.device_id)
                .map_err(|error| format!("cannot read the registrations: {error}"))?;
            Ok(Reachable::Targets(
                rows.into_iter()
                    .map(|(token, environment, bundle)| {
                        (announcement.device_id.clone(), token, environment, bundle)
                    })
                    .collect(),
            ))
        }
        NotifyScope::All => {
            // `all` is every paired device, not every token this host has ever
            // been handed. The push store outlives the `devices` rows it points
            // at, so a phone that was deleted or revoked leaves an active
            // registration behind, and reaching it would push into nothing
            // while claiming to have told every device.
            let rows = store
                .all_active()
                .map_err(|error| format!("cannot read the registrations: {error}"))?;
            let before = rows.len();
            let kept: Vec<Target> = rows
                .into_iter()
                .filter(|row| match live {
                    Some(live) => live.iter().any(|device| device == &row.0),
                    // The device list could not be read. Reaching nobody would
                    // be a silent turn; reaching every registration is the
                    // pre-filter behaviour, and the caller has already logged
                    // why.
                    None => true,
                })
                .collect();
            if kept.is_empty() && before > 0 {
                // Every token this host holds belongs to a device that has been
                // deleted or revoked. Saying so is the difference between an
                // operator who can fix it and one who wonders why the phone is
                // quiet.
                return Ok(Reachable::Nobody(
                    "the notify scope is all and no registration belongs to a paired device",
                ));
            }
            Ok(Reachable::Targets(kept))
        }
        NotifyScope::Targeted => {
            if announcement.devices.is_empty() {
                // A real answer, not a fallback: "chosen devices" with nothing
                // chosen means nobody, and saying so beats quietly widening to
                // every phone.
                return Ok(Reachable::Nobody(
                    "the notify scope is targeted and no device is named",
                ));
            }
            let mut targets = Vec::new();
            for device in &announcement.devices {
                // The same freshness check `all` makes. `save_push` refuses to
                // store a device that is not paired, but a device revoked after
                // it was chosen is still named in the setting, and a scope that
                // silently reaches a retired phone is worse than one that
                // reaches nobody.
                if let Some(live) = live
                    && !live.iter().any(|known| known == device)
                {
                    continue;
                }
                let rows = store
                    .active_registrations(device)
                    .map_err(|error| format!("cannot read the registrations: {error}"))?;
                targets.extend(rows.into_iter().map(|(token, environment, bundle)| {
                    (device.clone(), token, environment, bundle)
                }));
            }
            if targets.is_empty()
                && let Some(live) = live
                && !announcement
                    .devices
                    .iter()
                    .any(|device| live.iter().any(|known| known == device))
            {
                return Ok(Reachable::Nobody(
                    "the notify scope is targeted and no chosen device is still paired",
                ));
            }
            Ok(Reachable::Targets(targets))
        }
    }
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

/// Tell the devices an announcement resolves to that a turn finished.
///
/// `enabled` comes from the operator's setting and is passed rather than read
/// here, so the decision to notify is visible at the call site. The routing comes
/// from [`Announcement::scope`] and [`Announcement::devices`], and an `origin`
/// announcement reaches exactly the device it always did.
///
/// `live` is the paired, unrevoked device ids from the `devices` table, or
/// `None` when that list could not be read. `Some(empty)` means every device was
/// revoked or deleted, which is a scope that reaches nobody; `None` means the
/// filter is unavailable and the registrations are used as they come. It is
/// passed rather than read here because this module holds no handle on that
/// database. It bounds `all` and `targeted`: a registration whose device row is
/// gone is a token pointing at nothing.
pub async fn announce(
    sender: Option<Arc<ApnsSender>>,
    store: Option<&PushStore>,
    announcement: &Announcement,
    enabled: bool,
    live: Option<&[String]>,
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
    let targets = match reachable(store, announcement, live) {
        Ok(Reachable::Targets(targets)) => targets,
        Ok(Reachable::Nobody(reason)) => {
            // Logged rather than silent: an operator who chose a scope that
            // reaches nobody has to be able to find out why the phone was quiet.
            tracing::info!(
                job = %announcement.job_id,
                scope = announcement.scope.as_str(),
                "no push notification was sent: {reason}"
            );
            return Announced::Skipped(reason);
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the push registrations for a finished turn");
            return Announced::Failed(error);
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
    for (device_id, apns_token, environment, bundle) in targets {
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
                    let _ = store.set_environment(&device_id, &apns_token, proved);
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
                let _ = store.deactivate_token(&device_id, &apns_token);
                tracing::info!(
                    device_id = %device_id,
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
        let mut announcement = Announcement::to_origin(
            "device-1".to_owned(),
            "job-1".to_owned(),
            "The answer is 42.".to_owned(),
        );
        announcement.conversation = Some("conv-1".to_owned());
        announcement.message_id = Some("msg-1".to_owned());
        announcement
    }

    /// A registration for an arbitrary device, so a scope test can have more
    /// than one phone in the store.
    fn register_for(store: &PushStore, device: &str, token: &str) {
        store
            .upsert(
                device,
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
        let outcome = announce(None, Some(&store), &announcement(), true, Some(&live())).await;
        assert_eq!(outcome, Announced::Skipped("no APNs sender is configured"));
        assert!(!outcome.delivered());
    }

    #[tokio::test]
    async fn the_setting_off_wins_over_everything_else() {
        let store = store();
        let outcome = announce(
            sender().into(),
            Some(&store),
            &announcement(),
            false,
            Some(&live()),
        )
        .await;
        assert_eq!(outcome, Announced::Skipped("push.notify_on_reply is off"));
    }

    #[tokio::test]
    async fn a_turn_owned_by_no_device_is_not_announced() {
        let store = store();
        let mut announcement = announcement();
        announcement.device_id.clear();
        let outcome = announce(
            sender().into(),
            Some(&store),
            &announcement,
            true,
            Some(&live()),
        )
        .await;
        assert_eq!(
            outcome,
            Announced::Skipped("the notify scope is origin and the turn belongs to no device")
        );
    }

    #[tokio::test]
    async fn a_device_with_no_active_registration_is_reported_as_such() {
        let store = store();
        let outcome = announce(
            sender().into(),
            Some(&store),
            &announcement(),
            true,
            Some(&live()),
        )
        .await;
        assert_eq!(outcome, Announced::NoRegistration);
    }

    #[tokio::test]
    async fn a_retired_registration_is_not_attempted() {
        let store = store();
        register(&store, "token-1");
        assert_eq!(store.deactivate("device-1").expect("deactivate"), 1);
        let outcome = announce(
            sender().into(),
            Some(&store),
            &announcement(),
            true,
            Some(&live()),
        )
        .await;
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
        let outcome = announce(
            sender().into(),
            Some(&store),
            &announcement(),
            true,
            Some(&live()),
        )
        .await;
        assert_eq!(
            outcome,
            Announced::Failed("no registration carried a token".to_owned())
        );
    }

    /// The routing decision, with no delivery in it.
    ///
    /// Eight cases from the card, and each one is a fact about which phones ring
    /// rather than about how a banner looks. The store holds three devices and a
    /// retired fourth so `all` and a deactivated row are both covered.
    fn routing_store() -> PushStore {
        let store = store();
        register_for(&store, "device-1", "token-1");
        register_for(&store, "device-2", "token-2");
        register_for(&store, "device-3", "token-3");
        register_for(&store, "device-dead", "token-dead");
        store.deactivate("device-dead").expect("deactivate");
        store
    }

    /// The paired, unrevoked device ids the `devices` table would report for
    /// [`routing_store`]. Every registered device is live except the retired
    /// one, so a test that wants a stale registration has to say so.
    fn live() -> Vec<String> {
        vec![
            "device-1".to_owned(),
            "device-2".to_owned(),
            "device-3".to_owned(),
            "device-dead".to_owned(),
        ]
    }

    fn routed(announcement: &Announcement) -> Reachable {
        reachable(&routing_store(), announcement, Some(&live())).expect("the routing reads")
    }

    fn routed_devices(announcement: &Announcement) -> Vec<String> {
        match routed(announcement) {
            Reachable::Targets(targets) => targets.into_iter().map(|row| row.0).collect(),
            Reachable::Nobody(reason) => panic!("expected targets, got nobody: {reason}"),
        }
    }

    #[test]
    fn origin_reaches_only_the_device_that_ran_the_turn() {
        assert_eq!(routed_devices(&announcement()), vec!["device-1".to_owned()]);
    }

    #[test]
    fn origin_with_no_owning_device_reaches_nobody_and_says_why() {
        let mut announcement = announcement();
        announcement.device_id.clear();
        match routed(&announcement) {
            Reachable::Nobody(reason) => assert!(
                reason.contains("origin") && reason.contains("no device"),
                "the reason names the scope and the cause: {reason}"
            ),
            Reachable::Targets(targets) => {
                panic!("a browser turn must not reach a device: {targets:?}")
            }
        }
    }

    #[test]
    fn all_reaches_every_active_registration_and_never_a_retired_one() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::All;
        let devices = routed_devices(&announcement);
        assert_eq!(devices, vec!["device-1", "device-2", "device-3"]);
        assert!(
            !devices.iter().any(|device| device == "device-dead"),
            "a deactivated registration is never a target"
        );
    }

    /// The live case this filter exists for: an active token whose device row
    /// no longer exists. `push.db` and `eris.db` are separate files, so deleting
    /// a phone leaves its registration behind, and `all` used to push into it.
    #[test]
    fn all_never_reaches_a_registration_whose_device_is_gone() {
        let store = routing_store();
        register_for(&store, "device-orphan", "token-orphan");
        let mut announcement = announcement();
        announcement.scope = NotifyScope::All;
        let live = live();
        assert!(
            !live.iter().any(|device| device == "device-orphan"),
            "the fixture's device list must not carry the orphan"
        );
        match reachable(&store, &announcement, Some(&live)).expect("the routing reads") {
            Reachable::Targets(targets) => {
                let devices: Vec<String> = targets.into_iter().map(|row| row.0).collect();
                assert_eq!(devices, vec!["device-1", "device-2", "device-3"]);
            }
            Reachable::Nobody(reason) => panic!("expected targets: {reason}"),
        }
    }

    /// A device revoked after it was chosen is still named in the stored
    /// setting. `targeted` must not push at a phone the operator retired.
    #[test]
    fn targeted_skips_a_named_device_that_is_no_longer_paired() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::Targeted;
        announcement.devices = vec!["device-2".to_owned(), "device-3".to_owned()];
        let live = vec!["device-1".to_owned(), "device-2".to_owned()];
        match reachable(&routing_store(), &announcement, Some(&live)).expect("the routing reads") {
            Reachable::Targets(targets) => {
                let devices: Vec<String> = targets.into_iter().map(|row| row.0).collect();
                assert_eq!(devices, vec!["device-2"]);
            }
            Reachable::Nobody(reason) => panic!("expected targets: {reason}"),
        }
    }

    /// Every device revoked is a scope that reaches nobody, not a reason to
    /// fall back to the raw registration list.
    #[test]
    fn all_with_no_live_device_reaches_nobody() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::All;
        let none: Vec<String> = Vec::new();
        match reachable(&routing_store(), &announcement, Some(&none)).expect("the routing reads") {
            Reachable::Targets(targets) => {
                panic!("no paired device must reach nobody: {targets:?}")
            }
            Reachable::Nobody(_) => {}
        }
    }

    /// A device list that could not be read is not the same fact as "no
    /// devices": the announcement goes out rather than going silent.
    #[test]
    fn an_unreadable_device_list_falls_back_to_every_registration() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::All;
        match reachable(&routing_store(), &announcement, None).expect("the routing reads") {
            Reachable::Targets(targets) => {
                assert_eq!(targets.len(), 3, "every active registration is a target");
            }
            Reachable::Nobody(reason) => {
                panic!("an unreadable list must not reach nobody: {reason}")
            }
        }
    }

    #[test]
    fn targeted_reaches_the_named_devices_and_only_those() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::Targeted;
        announcement.devices = vec!["device-3".to_owned(), "device-2".to_owned()];
        let devices = routed_devices(&announcement);
        assert_eq!(devices, vec!["device-3", "device-2"]);
        assert!(!devices.iter().any(|device| device == "device-1"));
    }

    #[test]
    fn targeted_with_no_device_named_reaches_nobody() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::Targeted;
        match routed(&announcement) {
            Reachable::Nobody(reason) => assert!(reason.contains("targeted"), "{reason}"),
            Reachable::Targets(targets) => panic!("an empty list means nobody: {targets:?}"),
        }
    }

    #[test]
    fn a_targeted_device_that_is_not_registered_contributes_nothing() {
        let mut announcement = announcement();
        announcement.scope = NotifyScope::Targeted;
        announcement.devices = vec!["device-2".to_owned(), "device-unknown".to_owned()];
        assert_eq!(routed_devices(&announcement), vec!["device-2".to_owned()]);
    }

    #[test]
    fn a_scope_that_reaches_nobody_is_skipped_with_the_reason_and_not_sent() {
        let store = routing_store();
        let mut announcement = announcement();
        announcement.scope = NotifyScope::Targeted;
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(announce(
                Some(sender()),
                Some(&store),
                &announcement,
                true,
                Some(&live()),
            ));
        assert!(
            matches!(&outcome, Announced::Skipped(reason) if reason.contains("targeted")),
            "nothing is attempted: {outcome:?}"
        );
    }

    #[test]
    fn an_unknown_scope_means_origin_rather_than_a_broadcast() {
        assert_eq!(
            crate::push::notify_scope(Some("everything")),
            NotifyScope::Origin,
            "a corrupt row must not blast every phone"
        );
        assert_eq!(crate::push::notify_scope(None), NotifyScope::Origin);
        assert_eq!(crate::push::notify_scope(Some(" all ")), NotifyScope::All);
        assert_eq!(
            crate::push::notify_scope(Some("targeted")),
            NotifyScope::Targeted
        );
        assert_eq!(NotifyScope::All.as_str(), "all");
    }

    #[test]
    fn the_device_list_is_trimmed_deduplicated_and_empty_safe() {
        assert!(crate::push::notify_devices(None).is_empty());
        assert!(crate::push::notify_devices(Some("  ")).is_empty());
        assert_eq!(
            crate::push::notify_devices(Some("a, b,,a ,c")),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn the_master_switch_beats_every_scope() {
        let store = routing_store();
        let mut announcement = announcement();
        announcement.scope = NotifyScope::All;
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(announce(
                Some(sender()),
                Some(&store),
                &announcement,
                false,
                Some(&live()),
            ));
        assert_eq!(outcome, Announced::Skipped("push.notify_on_reply is off"));
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
