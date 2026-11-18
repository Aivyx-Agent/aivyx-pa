//! A20 — the board's IPC queries. Answered by the connection loop before
//! `handle_query` (the parked-steps model): a payload that isn't a board
//! query yields `None`.

use aivyx_ipc::protocol::{QueryPayload, QueryResponsePayload};

use super::model::BoardError;
use super::service::BoardService;

fn error(code: &str, message: String) -> QueryResponsePayload {
    QueryResponsePayload::QueryError {
        code: code.into(),
        message,
    }
}

fn board_error(e: &BoardError) -> QueryResponsePayload {
    let code = match e {
        BoardError::NotFound(_) => "board_not_found",
        BoardError::NotAllowed(_) => "board_not_allowed",
        BoardError::BadState { .. } => "board_bad_state",
        BoardError::Empty(_) => "board_empty",
        BoardError::Storage(_) => "board_storage",
    };
    error(code, e.to_string())
}

fn ack(r: Result<(), BoardError>) -> QueryResponsePayload {
    match r {
        Ok(()) => QueryResponsePayload::BoardAck,
        Err(e) => board_error(&e),
    }
}

fn created(r: Result<String, BoardError>) -> QueryResponsePayload {
    match r {
        Ok(id) => QueryResponsePayload::BoardCreated { id },
        Err(e) => board_error(&e),
    }
}

/// `None` for a payload that isn't a board query.
pub async fn handle_board_query(
    payload: &QueryPayload,
    board: Option<&BoardService>,
) -> Option<QueryResponsePayload> {
    let is_board = matches!(
        payload,
        QueryPayload::BoardSnapshot
            | QueryPayload::BoardTask { .. }
            | QueryPayload::BoardCreateGoal { .. }
            | QueryPayload::BoardCreateProject { .. }
            | QueryPayload::BoardCreateTask { .. }
            | QueryPayload::BoardAssign { .. }
            | QueryPayload::BoardStart { .. }
            | QueryPayload::BoardApprove { .. }
            | QueryPayload::BoardSendBack { .. }
            | QueryPayload::BoardCancel { .. }
            | QueryPayload::BoardComment { .. }
    );
    if !is_board {
        return None;
    }
    let Some(svc) = board else {
        return Some(error(
            "board_unavailable",
            "the work board isn't available on this daemon".into(),
        ));
    };
    Some(match payload {
        QueryPayload::BoardSnapshot => match svc.snapshot().await {
            Ok(snapshot) => QueryResponsePayload::Board { snapshot },
            Err(e) => board_error(&e),
        },
        QueryPayload::BoardTask { task_id } => match svc.task(task_id).await {
            Ok(task) => QueryResponsePayload::BoardTask { task },
            Err(e) => board_error(&e),
        },
        QueryPayload::BoardCreateGoal { title, description } => {
            created(svc.create_goal(title, description).await)
        }
        QueryPayload::BoardCreateProject {
            goal_id,
            title,
            description,
        } => created(svc.create_project(goal_id, title, description).await),
        QueryPayload::BoardCreateTask { task } => created(svc.create_task(task.clone()).await),
        QueryPayload::BoardAssign { task_id, owner } => {
            ack(svc.assign(task_id, owner.clone()).await)
        }
        QueryPayload::BoardStart { task_id } => match svc.start(task_id).await {
            Ok(mission_id) => QueryResponsePayload::BoardStarted { mission_id },
            Err(e) => board_error(&e),
        },
        QueryPayload::BoardApprove { task_id } => ack(svc.approve(task_id).await),
        QueryPayload::BoardSendBack { task_id, why } => ack(svc.send_back(task_id, why).await),
        QueryPayload::BoardCancel { task_id } => ack(svc.cancel(task_id).await),
        QueryPayload::BoardComment { task_id, text } => ack(svc.comment(task_id, text).await),
        _ => return None,
    })
}
