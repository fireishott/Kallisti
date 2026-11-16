//! Who answers a message in a room.
//!
//! The rule is small and it is the whole difference between a room and a group
//! chat that talks to itself: a `@mention` routes to the bots named, and with no
//! mention the room's policy decides - `everyone`, a named `lead`, or `mention`
//! (which means nobody answers until somebody is named).
//!
//! `auto` is deliberately not a fourth policy. "Automatic" is the feature that
//! produces a mention storm when two bots each decide the other should speak, and
//! the phase-3 success test is a room that answers *without* one. A room that
//! names its lead is predictable; a room that guesses is not.
//!
//! The turn limit is enforced where turns are counted, not here, but the policy
//! carries it so the limit and the routing rule live in one row.

use serde_json::json;

use crate::db::Db;

/// How a room decides who answers a message with no mention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Every member answers. Expensive, and what a small room wants.
    Everyone,
    /// Only the named lead answers.
    Lead,
    /// Nobody answers unless named. The safe default.
    Mention,
}

impl Policy {
    /// The spelling stored and sent.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Lead => "lead",
            Self::Mention => "mention",
        }
    }

    /// Read a policy, or `None` when the word is not one of the three.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "everyone" => Some(Self::Everyone),
            "lead" => Some(Self::Lead),
            "mention" => Some(Self::Mention),
            _ => None,
        }
    }
}

/// A room as the router needs it.
#[derive(Debug, Clone)]
pub struct Room {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// How an unmentioned message is routed.
    pub policy: Policy,
    /// The bot that answers under `lead`.
    pub lead: Option<String>,
    /// How many turns one message may spend before it needs an explicit continue.
    pub max_turns: i64,
    /// The bots in it, by id, in the order they were added.
    pub members: Vec<Member>,
}

/// One bot in a room.
#[derive(Debug, Clone)]
pub struct Member {
    /// The bot profile id.
    pub id: String,
    /// Its name, which is what a mention spells.
    pub name: String,
}

/// Who should answer.
///
/// Mentions win over policy: naming a bot is an instruction, and a policy that
/// overrode it would make `@name` a suggestion.
#[must_use]
pub fn route(room: &Room, mentions: &[String]) -> Vec<Member> {
    let wanted: Vec<String> = mentions
        .iter()
        .map(|name| name.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    // An `@` with no name is not a mention, so it falls through to the policy
    // rather than silencing the room.
    if !wanted.is_empty() {
        return room
            .members
            .iter()
            .filter(|member| wanted.contains(&member.name.to_ascii_lowercase()))
            .cloned()
            .collect();
    }
    match room.policy {
        Policy::Everyone => room.members.clone(),
        Policy::Lead => room
            .lead
            .as_ref()
            .and_then(|lead| room.members.iter().find(|member| &member.id == lead))
            .cloned()
            .into_iter()
            .collect(),
        Policy::Mention => Vec::new(),
    }
}

/// The pills a room shows, one per bot, with the state each is in.
///
/// `responded` and `passed` come from what the turn actually did; a bot this
/// round did not finish is `responding`. The pill list is derived rather than
/// stored, so it cannot drift from the turns.
#[must_use]
pub fn pills(room: &Room, responded: &[String], passed: &[String]) -> Vec<serde_json::Value> {
    room.members
        .iter()
        .map(|member| {
            let state = if responded.contains(&member.id) {
                "replied"
            } else if passed.contains(&member.id) {
                "passed"
            } else {
                "responding"
            };
            json!({"bot_id": member.id, "name": member.name, "state": state})
        })
        .collect()
}

/// Read one room, its members and its policy.
///
/// # Errors
///
/// The database's failure.
pub fn get(db: &Db, room_id: &str) -> Result<Option<Room>, crate::db::DbError> {
    db.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT id, name, policy, lead_id, max_turns FROM rooms WHERE id = ?1",
                rusqlite::params![room_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .ok();
        let Some((id, name, policy, lead, max_turns)) = row else {
            return Ok(None);
        };
        let mut statement = conn.prepare(
            "SELECT b.id, b.name FROM room_members m JOIN bot_profiles b ON b.id = m.bot_id \\
              WHERE m.room_id = ?1 ORDER BY b.name",
        )?;
        let members = statement
            .query_map(rusqlite::params![room_id], |row| {
                Ok(Member {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Room {
            id,
            name,
            policy: Policy::parse(&policy).unwrap_or(Policy::Mention),
            lead,
            max_turns,
            members,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::{Member, Policy, Room, route};

    fn room(policy: Policy, lead: Option<&str>) -> Room {
        Room {
            id: "room-1".to_owned(),
            name: "test".to_owned(),
            policy,
            lead: lead.map(str::to_owned),
            max_turns: 6,
            members: vec![
                Member {
                    id: "bot-a".to_owned(),
                    name: "Atlas".to_owned(),
                },
                Member {
                    id: "bot-b".to_owned(),
                    name: "Birch".to_owned(),
                },
            ],
        }
    }

    /// The default policy answers nobody until somebody is named: a room cannot
    /// start talking to itself.
    #[test]
    fn mention_policy_answers_only_a_named_bot() {
        let room = room(Policy::Mention, None);
        assert!(route(&room, &[]).is_empty());
        let named = route(&room, &["@atlas".to_owned()]);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "Atlas");
    }

    /// A mention is an instruction the policy does not override.
    #[test]
    fn a_mention_beats_the_policy() {
        let room = room(Policy::Everyone, None);
        let named = route(&room, &["birch".to_owned()]);
        assert_eq!(named.len(), 1, "only the mentioned bot, not everyone");
        assert_eq!(named[0].name, "Birch");
    }

    /// Everyone means everyone, lead means the lead, and a lead that is not a
    /// member answers nobody rather than guessing.
    #[test]
    fn the_other_two_policies_route_to_the_right_members() {
        assert_eq!(route(&room(Policy::Everyone, None), &[]).len(), 2);
        let led = route(&room(Policy::Lead, Some("bot-b")), &[]);
        assert_eq!(led.len(), 1);
        assert_eq!(led[0].name, "Birch");
        assert!(route(&room(Policy::Lead, Some("bot-missing")), &[]).is_empty());
    }

    /// An unknown name is not answered, and a mention of nobody is not a mention.
    #[test]
    fn an_unknown_mention_routes_to_nobody() {
        let room = room(Policy::Everyone, None);
        assert!(route(&room, &["@nobody".to_owned()]).is_empty());
        assert_eq!(route(&room, &["@".to_owned()]).len(), 2, "an empty mention falls back to policy");
    }

    /// A pill says what each bot did, and a bot that did neither is still working.
    #[test]
    fn pills_report_replied_passed_and_responding() {
        let room = room(Policy::Everyone, None);
        let first = super::pills(&room, &["bot-a".to_owned()], &[]);
        assert_eq!(first[0]["state"], "replied");
        assert_eq!(first[1]["state"], "responding");
        let passed = super::pills(&room, &[], &["bot-b".to_owned()]);
        assert_eq!(passed[1]["state"], "passed");
    }
}
