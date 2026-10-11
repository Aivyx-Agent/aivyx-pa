//! Who may drive a turn. Pure, so every case is a unit test.
//!
//! A message is a turn only from an `allowed_senders` ID, in the
//! `room_filter` room, sent from a verified device. The operator writing
//! from an unverified device, or sending something the assistant can't
//! decrypt, gets one explanation per room; anyone else is ignored.

use std::collections::HashSet;

use crate::transport::{Inbound, IncomingMessage};

/// Told once per room when the operator writes from an unverified device.
pub const ASK_TO_VERIFY: &str = "I only answer your verified devices. Run \
    `aivyx-pa matrix verify` and verify this device from your Matrix app.";

/// Told once per room when a message from the operator can't be decrypted.
pub const CANT_DECRYPT: &str = "I couldn't read that message. Verify my device \
    with `aivyx-pa matrix verify`, then send it again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixPolicy {
    pub room_filter: String,
    pub allowed_senders: Vec<String>,
}

impl MatrixPolicy {
    fn is_operator(&self, sender: &str) -> bool {
        self.allowed_senders.iter().any(|s| s == sender)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Turn(IncomingMessage),
    AskToVerify { room_id: String },
    CantDecrypt { room_id: String },
    Join { room_id: String },
    Decline { room_id: String },
    Ignore,
}

pub fn decide(policy: &MatrixPolicy, inbound: Inbound) -> Action {
    match inbound {
        Inbound::Message(m) => {
            if m.room_id != policy.room_filter || !policy.is_operator(&m.sender) {
                Action::Ignore
            } else if m.verified {
                Action::Turn(m)
            } else {
                Action::AskToVerify { room_id: m.room_id }
            }
        }
        Inbound::Undecryptable { room_id, sender } => {
            if room_id == policy.room_filter && policy.is_operator(&sender) {
                Action::CantDecrypt { room_id }
            } else {
                Action::Ignore
            }
        }
        // Joined only when it is the operator inviting the assistant to the
        // home room; the assistant has no use for any other room.
        Inbound::Invite { room_id, inviter } => {
            if policy.is_operator(&inviter) && room_id == policy.room_filter {
                Action::Join { room_id }
            } else {
                Action::Decline { room_id }
            }
        }
    }
}

/// Remembers which rooms already got a given explanation this run.
#[derive(Debug, Default)]
pub struct OncePerRoom(HashSet<String>);

impl OncePerRoom {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` the first time `room_id` is seen.
    pub fn first(&mut self, room_id: &str) -> bool {
        self.0.insert(room_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> MatrixPolicy {
        MatrixPolicy {
            room_filter: "!home:x".into(),
            allowed_senders: vec!["@me:x".into()],
        }
    }

    fn message(room: &str, sender: &str, verified: bool) -> Inbound {
        Inbound::Message(IncomingMessage {
            room_id: room.into(),
            sender: sender.into(),
            text: "hi".into(),
            verified,
        })
    }

    #[test]
    fn only_the_operator_from_a_verified_device_in_the_home_room_is_a_turn() {
        assert!(matches!(
            decide(&policy(), message("!home:x", "@me:x", true)),
            Action::Turn(_)
        ));
        // Every other combination is not.
        assert_eq!(
            decide(&policy(), message("!home:x", "@me:x", false)),
            Action::AskToVerify {
                room_id: "!home:x".into()
            }
        );
        assert_eq!(
            decide(&policy(), message("!home:x", "@stranger:x", true)),
            Action::Ignore
        );
        assert_eq!(
            decide(&policy(), message("!other:x", "@me:x", true)),
            Action::Ignore
        );
        assert_eq!(
            decide(&policy(), message("!other:x", "@stranger:x", false)),
            Action::Ignore
        );
    }

    #[test]
    fn undecryptable_is_answered_only_for_the_operator_in_the_home_room() {
        let ud = |room: &str, sender: &str| Inbound::Undecryptable {
            room_id: room.into(),
            sender: sender.into(),
        };
        assert_eq!(
            decide(&policy(), ud("!home:x", "@me:x")),
            Action::CantDecrypt {
                room_id: "!home:x".into()
            }
        );
        assert_eq!(
            decide(&policy(), ud("!home:x", "@stranger:x")),
            Action::Ignore
        );
        assert_eq!(decide(&policy(), ud("!other:x", "@me:x")), Action::Ignore);
    }

    #[test]
    fn invites_are_accepted_only_from_allowed_senders_to_the_home_room() {
        let inv = |room: &str, inviter: &str| Inbound::Invite {
            room_id: room.into(),
            inviter: inviter.into(),
        };
        assert_eq!(
            decide(&policy(), inv("!home:x", "@me:x")),
            Action::Join {
                room_id: "!home:x".into()
            }
        );
        assert_eq!(
            decide(&policy(), inv("!home:x", "@stranger:x")),
            Action::Decline {
                room_id: "!home:x".into()
            }
        );
        assert_eq!(
            decide(&policy(), inv("!other:x", "@me:x")),
            Action::Decline {
                room_id: "!other:x".into()
            }
        );
    }

    #[test]
    fn once_per_room_answers_each_room_once() {
        let mut once = OncePerRoom::new();
        assert!(once.first("!a:x"));
        assert!(!once.first("!a:x"));
        assert!(once.first("!b:x"));
    }
}
