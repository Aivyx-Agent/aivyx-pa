//! The `/team` command chain the Matrix and Proton daemon frontends share
//! (sender gate → pending yes/no confirmation → `/team run` opt-in and rate
//! limit → other `/team` subcommands). Lifted from
//! `matrix_daemon_frontend` so both use one copy.

use std::path::Path;
use std::time::Instant;

use crate::daemon_client;
use crate::daemon_ipc::FrontendType;
use crate::team_command::{self, TeamCommand, sender_allowed};
use crate::team_dispatch;
use crate::team_trigger_state::{
    ConfirmReply, PendingTrigger, check_and_record_trigger, parse_confirm_reply,
};

/// The `/team` precedence chain for one message, shared by the Matrix and
/// Proton daemon frontends. `Some(reply)` when it
/// was a team command or a pending confirmation; `None` to carry on.
#[allow(clippy::too_many_arguments)]
pub async fn handle_team_command(
    frontend: FrontendType,
    text: &str,
    socket_path: &Path,
    pending: &mut Option<PendingTrigger<String>>,
    history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender: String,
    allowed_senders: &[String],
) -> Option<String> {
    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender) {
        return Some("✗ you are not authorized to issue /team commands.".to_string());
    }
    if let Some(p) = pending.take() {
        let now = Instant::now();
        match parse_confirm_reply(text) {
            Some(ConfirmReply::Yes) | Some(ConfirmReply::No) if p.sender_id != sender => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
            Some(ConfirmReply::Yes) if p.is_expired(now) => {
                return Some("✗ that request expired, ask again.".to_string());
            }
            Some(ConfirmReply::Yes) => {
                return Some(
                    match daemon_client::run_team_mission_channel(
                        socket_path,
                        frontend,
                        p.goal.clone(),
                    )
                    .await
                    {
                        Ok(id) => format!("✓ Started mission {id}."),
                        Err(e) => format!("✗ Could not start the mission: {e}"),
                    },
                );
            }
            Some(ConfirmReply::No) => return Some("Cancelled.".to_string()),
            None => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
        }
    }
    if let Some(TeamCommand::Run { goal }) = team_command::parse(text) {
        if !team_run_channel {
            return Some("✗ this channel is not authorized to start team missions.".to_string());
        }
        if let Some(limit) = team_trigger_rate_limit
            && !check_and_record_trigger(history, limit, Instant::now())
        {
            return Some(format!(
                "✗ too many mission-start requests (max {limit} per hour), try again later."
            ));
        }
        *pending = Some(PendingTrigger::new(goal.clone(), sender));
        return Some(format!("Start '{goal}' on the default team? Reply yes/no."));
    }
    if let Some(cmd) = team_command::parse(text) {
        return Some(team_dispatch::dispatch(socket_path, cmd).await);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_team_command_from_a_sender_not_allowed_is_refused() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_team_command(
            FrontendType::Matrix,
            "/team status",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@stranger:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(
            out,
            Some("✗ you are not authorized to issue /team commands.".to_string())
        );
    }

    #[tokio::test]
    async fn ordinary_text_is_not_a_team_command() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_team_command(
            FrontendType::Matrix,
            "hello",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@me:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(out, None);
    }
}
