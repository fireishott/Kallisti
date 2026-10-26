//! Auxiliary model roles.
//!
//! A turn answers with the primary model. Everything beside it - reading an
//! image, naming a conversation, compressing a long context - is a separate job
//! that does not want the same model, and often wants a cheaper one.
//!
//! This module is the registry of those roles and the storage for their choice.
//! It deliberately does not decide what a role runs: that is the caller's job, so
//! a role can be added here before anything consumes it and the settings page can
//! still say plainly which roles a turn actually uses today.

/// The value that means "whatever the primary model is".
pub const INHERIT: &str = "primary";

/// One auxiliary job a model can be assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    /// Stable key, also the settings-row segment.
    pub key: &'static str,
    /// What the operator reads.
    pub label: &'static str,
    /// What the job does, in one line.
    pub purpose: &'static str,
    /// Whether the stored model must appear in the chat-model probe.
    ///
    /// False for a role that does not call a chat endpoint: the image role posts
    /// to an image endpoint, so a chat-model catalogue is the wrong list to hold
    /// it to and refusing the pin would be checking the wrong thing.
    pub probe_validated: bool,
    /// What consumes it. Not optional on purpose: a settings row that looks wired
    /// and is not is the same lie as a button that does nothing, so a role with no
    /// consumer cannot be declared at all.
    pub consumed_by: &'static str,
}

/// Every role this build knows about, in the order the page lists them.
pub const ROLES: &[Role] = &[
    Role {
        key: "vision",
        label: "Vision",
        probe_validated: true,
        purpose: "Reads an image attached to a turn.",
        consumed_by: ("a turn that carries an image, once this role is pinned"),
    },
    Role {
        key: "image",
        label: "Image generation",
        purpose: "Draws an image when a turn asks for one.",
        probe_validated: false,
        consumed_by: ("a turn that asks for a picture, once this role is pinned"),
    },
    Role {
        key: "web_extract",
        label: "Web extract",
        probe_validated: true,
        purpose: "Pulls the readable text out of a fetched page.",
        consumed_by: ("a turn that fetches a URL, once this role is pinned"),
    },
    Role {
        key: "compression",
        label: "Compression",
        probe_validated: true,
        purpose: "Summarises context that no longer fits the window.",
        consumed_by: ("a turn whose history has outgrown the window, once this role is pinned"),
    },
    Role {
        key: "title_generation",
        label: "Title generation",
        probe_validated: true,
        purpose: "Names a conversation from its first exchange.",
        consumed_by: ("a settled turn, once this role is pinned to a model"),
    },
    Role {
        key: "approval",
        label: "Approval",
        probe_validated: true,
        purpose: "Reads a parked tool call and says what it would do.",
        consumed_by: ("a call parked for approval, once this role is pinned"),
    },
    Role {
        key: "fallback",
        label: "Fallback",
        probe_validated: true,
        purpose: "Retries a failed turn on another model.",
        consumed_by: ("a turn whose primary model failed, once this role is pinned to a model"),
    },
];

/// The `settings` row that holds one role's model id.
#[must_use]
pub fn setting_key(role: &str) -> String {
    format!("aux.{role}.model")
}

/// The `settings` row that holds one role's provider id.
#[must_use]
pub fn provider_key(role: &str) -> String {
    format!("aux.{role}.provider")
}

/// The provider a role should run on.
///
/// An unset row, or the inherit sentinel, follows the primary provider. An id
/// that is no longer in the registry falls back to the primary rather than
/// disabling the role, and the settings page shows the id it stored so the
/// mismatch is visible.
#[must_use]
pub fn provider_for<'a>(
    stored: Option<&str>,
    registry: &'a [crate::providers::Provider],
    primary: &str,
) -> Option<&'a crate::providers::Provider> {
    match stored.map(str::trim) {
        None | Some("") | Some(INHERIT) => crate::providers::resolve(registry, primary),
        Some(id) => registry
            .iter()
            .find(|provider| provider.id == id)
            .or_else(|| crate::providers::resolve(registry, primary)),
    }
}

/// The role with this key, when this build knows it.
#[must_use]
pub fn role(key: &str) -> Option<&'static Role> {
    ROLES.iter().find(|role| role.key == key)
}

/// The model a role should use, given the primary one.
///
/// An empty row and the literal [`INHERIT`] mean the same thing: follow the
/// primary model. A role that was explicitly pointed somewhere keeps that
/// choice even when the primary moves.
#[must_use]
pub fn effective(stored: Option<&str>, primary: &str) -> String {
    match stored.map(str::trim) {
        None | Some("") | Some(INHERIT) => primary.to_owned(),
        Some(model) => model.to_owned(),
    }
}

/// Whether a role currently follows the primary model.
#[must_use]
pub fn inherits(stored: Option<&str>) -> bool {
    matches!(stored.map(str::trim), None | Some("") | Some(INHERIT))
}

/// Normalize what the form submitted: the sentinel is stored, not a model name.
#[must_use]
pub fn store_value(submitted: &str) -> String {
    let trimmed = submitted.trim();
    if trimmed.is_empty() || trimmed == INHERIT {
        INHERIT.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_role_follows_the_primary_model() {
        assert_eq!(effective(None, "ds/one"), "ds/one");
        assert_eq!(effective(Some(""), "ds/one"), "ds/one");
        assert_eq!(effective(Some(INHERIT), "ds/one"), "ds/one");
        assert!(inherits(None));
        assert!(inherits(Some(INHERIT)));
    }

    #[test]
    fn a_role_pinned_to_a_model_keeps_it_when_the_primary_moves() {
        assert_eq!(effective(Some("ag/vision"), "ds/one"), "ag/vision");
        assert!(!inherits(Some("ag/vision")));
    }

    #[test]
    fn the_form_sentinel_is_stored_as_the_sentinel() {
        assert_eq!(store_value(""), INHERIT);
        assert_eq!(store_value("  "), INHERIT);
        assert_eq!(store_value(INHERIT), INHERIT);
        assert_eq!(store_value(" ag/vision "), "ag/vision");
    }

    #[test]
    fn every_role_key_is_unique_and_addresses_its_own_row() {
        let mut keys: Vec<&str> = ROLES.iter().map(|role| role.key).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "two roles share a key");
        assert_eq!(setting_key("vision"), "aux.vision.model");
        assert!(role("vision").is_some());
        assert!(role("nope").is_none());
    }

    #[test]
    fn a_role_follows_the_primary_provider_until_it_is_pointed_elsewhere() {
        use crate::providers::Provider;
        let registry = vec![
            Provider {
                id: "alpha".to_owned(),
                name: "Alpha".to_owned(),
                base_url: "http://a/v1".to_owned(),
                key_ref: "env:A".to_owned(),
                dialect: None,
                reasoning_split: None,
                direct: None,
                project: None,
            },
            Provider {
                id: "beta".to_owned(),
                name: "Beta".to_owned(),
                base_url: "http://b/v1".to_owned(),
                key_ref: "env:B".to_owned(),
                dialect: None,
                reasoning_split: None,
                direct: None,
                project: None,
            },
        ];

        assert_eq!(
            provider_for(None, &registry, "alpha").map(|p| p.id.as_str()),
            Some("alpha")
        );
        assert_eq!(
            provider_for(Some("beta"), &registry, "alpha").map(|p| p.id.as_str()),
            Some("beta")
        );
        assert_eq!(
            provider_for(Some("gone"), &registry, "alpha").map(|p| p.id.as_str()),
            Some("alpha"),
            "a provider that left the registry falls back rather than breaking the role"
        );
        assert_eq!(provider_key("vision"), "aux.vision.provider");
    }

    #[test]
    fn only_the_image_role_skips_the_chat_model_probe() {
        for role in ROLES {
            assert_eq!(
                role.probe_validated,
                role.key != "image",
                "{} is validated against the wrong catalogue",
                role.key
            );
        }
    }

    #[test]
    fn every_offered_role_names_what_reads_it() {
        // The list is the contract: a row exists only because something calls it.
        let keys: Vec<&str> = ROLES.iter().map(|role| role.key).collect();
        assert_eq!(
            keys,
            vec![
                "vision",
                "image",
                "web_extract",
                "compression",
                "title_generation",
                "approval",
                "fallback"
            ]
        );
        for role in ROLES {
            assert!(
                !role.consumed_by.trim().is_empty(),
                "{} is offered with no consumer, which is a decorative row",
                role.key
            );
        }
    }

    #[test]
    fn a_role_nothing_reads_is_not_offered_at_all() {
        // These were in the table with `consumed_by: None` and rendered as
        // "Nothing reads this row yet". That is a placeholder in a settings page,
        // so they are gone until a feature needs them.
        for gone in [
            "mcp",
            "skills_hub",
            "triage_specifier",
            "kanban_decomposer",
            "profile_describer",
            "curator",
        ] {
            assert!(
                ROLES.iter().all(|role| role.key != gone),
                "{gone} is still offered with nothing behind it"
            );
        }
    }
}
