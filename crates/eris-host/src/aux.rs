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
    /// What consumes it today, or `None` when nothing does yet.
    ///
    /// Stated rather than implied: a settings row that looks wired and is not is
    /// the same lie as a button that does nothing.
    pub consumed_by: Option<&'static str>,
}

/// Every role this build knows about, in the order the page lists them.
pub const ROLES: &[Role] = &[
    Role {
        key: "vision",
        label: "Vision",
        purpose: "Reads an image attached to a turn.",
        consumed_by: None,
    },
    Role {
        key: "image",
        label: "Image generation",
        purpose: "Draws an image when a turn asks for one.",
        consumed_by: None,
    },
    Role {
        key: "web_extract",
        label: "Web extract",
        purpose: "Pulls the readable text out of a fetched page.",
        consumed_by: None,
    },
    Role {
        key: "compression",
        label: "Compression",
        purpose: "Summarises context that no longer fits the window.",
        consumed_by: Some("a turn whose history has outgrown the window, once this role is pinned"),
    },
    Role {
        key: "title_generation",
        label: "Title generation",
        purpose: "Names a conversation from its first exchange.",
        consumed_by: Some("a settled turn, once this role is pinned to a model"),
    },
    Role {
        key: "approval",
        label: "Approval",
        purpose: "Judges a parked tool call before it runs.",
        consumed_by: None,
    },
    Role {
        key: "mcp",
        label: "MCP",
        purpose: "Reasons over a tool result returned by an MCP server.",
        consumed_by: None,
    },
    Role {
        key: "skills_hub",
        label: "Skills hub",
        purpose: "Picks the skill that fits a request.",
        consumed_by: None,
    },
    Role {
        key: "triage_specifier",
        label: "Triage",
        purpose: "Classifies an inbound request.",
        consumed_by: None,
    },
    Role {
        key: "kanban_decomposer",
        label: "Kanban decomposer",
        purpose: "Breaks a request into tasks.",
        consumed_by: None,
    },
    Role {
        key: "profile_describer",
        label: "Profile describer",
        purpose: "Writes the one-line description of something.",
        consumed_by: None,
    },
    Role {
        key: "curator",
        label: "Curator",
        purpose: "Files and links notes.",
        consumed_by: None,
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
            },
            Provider {
                id: "beta".to_owned(),
                name: "Beta".to_owned(),
                base_url: "http://b/v1".to_owned(),
                key_ref: "env:B".to_owned(),
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
    fn only_the_roles_with_a_real_consumer_claim_one() {
        // The day another role is wired, this list is the thing to edit - and
        // editing it is a reminder to update the page's honesty note too.
        let wired: Vec<&str> = ROLES
            .iter()
            .filter(|role| role.consumed_by.is_some())
            .map(|role| role.key)
            .collect();
        assert_eq!(wired, vec!["compression", "title_generation"]);
    }
}
