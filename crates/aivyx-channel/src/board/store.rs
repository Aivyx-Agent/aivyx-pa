//! The board's records in `KeyDomain::Board`: `goal\0<id>`, `project\0<id>`,
//! `task\0<id>`, `agent\0<id>` (A21), each a JSON value.

use aivyx_ipc::board::{Agent, Goal, Project, Task};
use aivyx_storage::{DomainHandle, KeyDomain, StorageError};
use serde::{Serialize, de::DeserializeOwned};

fn key(kind: &str, id: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(kind.len() + 1 + id.len());
    k.extend_from_slice(kind.as_bytes());
    k.push(0);
    k.extend_from_slice(id.as_bytes());
    k
}

async fn put<T: Serialize>(
    h: &DomainHandle,
    kind: &str,
    id: &str,
    v: &T,
) -> Result<(), StorageError> {
    debug_assert_eq!(h.domain(), KeyDomain::Board);
    let json = serde_json::to_vec(v)
        .map_err(|e| StorageError::Redb(format!("serialize board {kind}: {e}")))?;
    h.put(&key(kind, id), &json).await
}

async fn get<T: DeserializeOwned>(
    h: &DomainHandle,
    kind: &str,
    id: &str,
) -> Result<Option<T>, StorageError> {
    match h.get(&key(kind, id)).await? {
        None => Ok(None),
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| StorageError::Redb(format!("decode board {kind} {id}: {e}"))),
    }
}

async fn list<T: DeserializeOwned>(h: &DomainHandle, kind: &str) -> Result<Vec<T>, StorageError> {
    let mut prefix = kind.as_bytes().to_vec();
    prefix.push(0);
    let mut out = Vec::new();
    for (_, bytes) in h.scan_prefix(&prefix).await? {
        out.push(
            serde_json::from_slice(&bytes)
                .map_err(|e| StorageError::Redb(format!("decode board {kind}: {e}")))?,
        );
    }
    Ok(out)
}

pub async fn put_goal(h: &DomainHandle, g: &Goal) -> Result<(), StorageError> {
    put(h, "goal", &g.id, g).await
}
pub async fn get_goal(h: &DomainHandle, id: &str) -> Result<Option<Goal>, StorageError> {
    get(h, "goal", id).await
}
pub async fn list_goals(h: &DomainHandle) -> Result<Vec<Goal>, StorageError> {
    list(h, "goal").await
}
pub async fn put_project(h: &DomainHandle, p: &Project) -> Result<(), StorageError> {
    put(h, "project", &p.id, p).await
}
pub async fn get_project(h: &DomainHandle, id: &str) -> Result<Option<Project>, StorageError> {
    get(h, "project", id).await
}
pub async fn list_projects(h: &DomainHandle) -> Result<Vec<Project>, StorageError> {
    list(h, "project").await
}
pub async fn put_task(h: &DomainHandle, t: &Task) -> Result<(), StorageError> {
    put(h, "task", &t.id, t).await
}
pub async fn get_task(h: &DomainHandle, id: &str) -> Result<Option<Task>, StorageError> {
    get(h, "task", id).await
}
pub async fn list_tasks(h: &DomainHandle) -> Result<Vec<Task>, StorageError> {
    list(h, "task").await
}

pub async fn put_agent(h: &DomainHandle, a: &Agent) -> Result<(), StorageError> {
    put(h, "agent", &a.id, a).await
}

pub async fn get_agent(h: &DomainHandle, id: &str) -> Result<Option<Agent>, StorageError> {
    get(h, "agent", id).await
}

pub async fn list_agents(h: &DomainHandle) -> Result<Vec<Agent>, StorageError> {
    list(h, "agent").await
}

pub async fn delete_agent(h: &DomainHandle, id: &str) -> Result<(), StorageError> {
    h.delete(&key("agent", id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_crypto::MasterKey;
    use aivyx_ipc::board::TaskState;
    use aivyx_storage::{RedbStorage, Storage, StorageConfig};
    use std::sync::Arc;

    async fn open(dir: &std::path::Path, key: u8) -> Arc<dyn Storage> {
        RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([key; 32]),
        )
        .await
        .unwrap()
    }

    fn goal(id: &str) -> Goal {
        Goal {
            id: id.into(),
            title: "G".into(),
            description: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            goal_id: "g-1".into(),
            title: "P".into(),
            description: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            project_id: "p-1".into(),
            parent_task: None,
            title: "T".into(),
            description: String::new(),
            owner: None,
            review_required: true,
            state: TaskState::Todo,
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn put_get_list_round_trip_for_each_kind() {
        let dir = tempfile::tempdir().unwrap();
        let h = open(dir.path(), 1).await.domain(KeyDomain::Board);

        assert_eq!(get_goal(&h, "g-1").await.unwrap(), None);
        put_goal(&h, &goal("g-1")).await.unwrap();
        put_goal(&h, &goal("g-2")).await.unwrap();
        assert_eq!(get_goal(&h, "g-1").await.unwrap(), Some(goal("g-1")));
        assert_eq!(list_goals(&h).await.unwrap().len(), 2);

        put_project(&h, &project("p-1")).await.unwrap();
        assert_eq!(get_project(&h, "p-1").await.unwrap(), Some(project("p-1")));
        assert_eq!(list_projects(&h).await.unwrap(), vec![project("p-1")]);

        put_task(&h, &task("t-1")).await.unwrap();
        let mut t = task("t-1");
        t.title = "changed".into();
        put_task(&h, &t).await.unwrap();
        assert_eq!(get_task(&h, "t-1").await.unwrap(), Some(t.clone()));
        assert_eq!(list_tasks(&h).await.unwrap(), vec![t]);
    }

    #[tokio::test]
    async fn kinds_with_the_same_id_do_not_collide_and_lists_are_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let h = open(dir.path(), 1).await.domain(KeyDomain::Board);
        put_goal(&h, &goal("x-1")).await.unwrap();
        put_project(&h, &project("x-1")).await.unwrap();
        put_task(&h, &task("x-1")).await.unwrap();
        assert_eq!(get_goal(&h, "x-1").await.unwrap().unwrap().title, "G");
        assert_eq!(get_project(&h, "x-1").await.unwrap().unwrap().title, "P");
        assert_eq!(get_task(&h, "x-1").await.unwrap().unwrap().title, "T");
        assert_eq!(list_tasks(&h).await.unwrap().len(), 1);
        assert_eq!(list_goals(&h).await.unwrap().len(), 1);
        assert_eq!(list_projects(&h).await.unwrap().len(), 1);
    }

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: "scout".into(),
            title: String::new(),
            reports_to: None,
            works_as: aivyx_ipc::board::WorksAs::Team,
            heartbeat: None,
            wake_on_assign: true,
            paused: false,
            last_wake: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn agents_round_trip_delete_and_never_collide_with_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let h = open(dir.path(), 1).await.domain(KeyDomain::Board);
        assert_eq!(get_agent(&h, "x-1").await.unwrap(), None);
        put_agent(&h, &agent("x-1")).await.unwrap();
        put_task(&h, &task("x-1")).await.unwrap();
        assert_eq!(get_agent(&h, "x-1").await.unwrap(), Some(agent("x-1")));
        assert_eq!(get_task(&h, "x-1").await.unwrap().unwrap().title, "T");
        assert_eq!(list_agents(&h).await.unwrap(), vec![agent("x-1")]);
        assert_eq!(list_tasks(&h).await.unwrap().len(), 1);
        delete_agent(&h, "x-1").await.unwrap();
        assert_eq!(get_agent(&h, "x-1").await.unwrap(), None);
        assert!(list_agents(&h).await.unwrap().is_empty());
        assert_eq!(list_tasks(&h).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reopening_with_the_same_key_reads_the_rows() {
        let dir = tempfile::tempdir().unwrap();
        {
            let h = open(dir.path(), 7).await.domain(KeyDomain::Board);
            put_task(&h, &task("t-9")).await.unwrap();
        }
        let h = open(dir.path(), 7).await.domain(KeyDomain::Board);
        assert_eq!(get_task(&h, "t-9").await.unwrap(), Some(task("t-9")));
    }
}
