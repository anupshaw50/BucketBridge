//! Per-operation options of queued transfers, and the broker that carries
//! overwrite questions (policy `ask`) from running tasks to the user.
//!
//! The options of a call (`enqueue_*`) ride in the group ID of its tasks, as
//! a suffix (`<uuid>~ie=skip;mt=0;da=<account>`), so they survive a restart
//! with the queue and need no new field in the task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use super::policy::{Choice, Conflict, ConflictResolver};
use crate::error::{Error, Result};
use crate::settings::IfExists;

/// Separates the options from the base of a group ID.
const MARKER: char = '~';

/// Most groups whose "apply to all" answer is remembered.
const MAX_REMEMBERED: usize = 256;

/// What one enqueue call asks for besides the selection. Every field
/// defaults to the settings (and to the source account for the destination).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobOptions {
    /// The overwrite policy of this call; `None` uses the `transfer`
    /// setting.
    pub if_exists: Option<IfExists>,
    /// Keep modification times; `None` uses the `data_integrity` setting.
    pub preserve_mtime: Option<bool>,
    /// The account that owns the destination of a copy or move; `None` is
    /// the account of the source (a copy within one account).
    pub dst_account: Option<String>,
}

impl JobOptions {
    /// Checks the options.
    pub fn validate(&self) -> Result<()> {
        if let Some(account) = &self.dst_account
            && (account.is_empty() || account.contains([MARKER, ';', '=']))
        {
            return Err(Error::Usage(format!(
                "'{account}' is not a usable destination account id"
            )));
        }
        Ok(())
    }

    /// `base` with these options appended; `base` itself when there are
    /// none.
    pub fn group_id(&self, base: String) -> String {
        let mut parts = Vec::new();
        if let Some(policy) = self.if_exists {
            parts.push(format!("ie={}", policy.as_str()));
        }
        if let Some(keep) = self.preserve_mtime {
            parts.push(format!("mt={}", u8::from(keep)));
        }
        if let Some(account) = &self.dst_account {
            parts.push(format!("da={account}"));
        }
        if parts.is_empty() {
            base
        } else {
            format!("{base}{MARKER}{}", parts.join(";"))
        }
    }

    /// The options a group ID carries (none for an ordinary group).
    pub fn of_group(group_id: Option<&str>) -> JobOptions {
        let mut options = JobOptions::default();
        let Some((_, rest)) = group_id.and_then(|group| group.rsplit_once(MARKER)) else {
            return options;
        };
        for part in rest.split(';') {
            match part.split_once('=') {
                Some(("ie", value)) => options.if_exists = value.parse().ok(),
                Some(("mt", value)) => options.preserve_mtime = Some(value == "1"),
                Some(("da", value)) if !value.is_empty() => {
                    options.dst_account = Some(value.to_owned());
                }
                _ => {}
            }
        }
        options
    }
}

/// A question waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingConflict {
    /// The task that waits.
    pub task_id: String,
    /// Its group (the batch an "apply to all" answer covers).
    pub group_id: Option<String>,
    /// What exists where.
    pub conflict: Conflict,
}

/// A change in the set of waiting questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConflictEvent {
    /// A task started to wait for an answer.
    Asked(PendingConflict),
    /// A task no longer waits (answered, stopped or cancelled).
    Resolved {
        /// The task.
        task_id: String,
    },
}

struct Waiting {
    pending: PendingConflict,
    answer: oneshot::Sender<Choice>,
}

#[derive(Default)]
struct State {
    waiting: HashMap<String, Waiting>,
    /// Answers given with "apply to all remaining in this batch".
    remembered: Vec<(String, Choice)>,
    listeners: Vec<mpsc::UnboundedSender<ConflictEvent>>,
}

impl State {
    fn emit(&mut self, event: ConflictEvent) {
        self.listeners
            .retain(|listener| listener.send(event.clone()).is_ok());
    }

    fn remembered(&self, group: &str) -> Option<Choice> {
        self.remembered
            .iter()
            .find(|(known, _)| known == group)
            .map(|(_, choice)| *choice)
    }
}

/// Carries the overwrite questions of running tasks to whoever can answer
/// them (the desktop host turns them into events and a dialog). Cheap to
/// clone; clones share the questions.
#[derive(Clone, Default)]
pub struct ConflictBroker {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for ConflictBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConflictBroker")
    }
}

impl ConflictBroker {
    /// A broker without questions.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Events from now on.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<ConflictEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.lock().listeners.push(sender);
        receiver
    }

    /// The questions waiting right now, oldest task id first.
    pub fn pending(&self) -> Vec<PendingConflict> {
        let state = self.lock();
        let mut all: Vec<PendingConflict> = state
            .waiting
            .values()
            .map(|waiting| waiting.pending.clone())
            .collect();
        all.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        all
    }

    /// Answers the question of `task_id`. With `apply_to_batch` the answer
    /// also settles every other waiting question of the same group and every
    /// later one of it. `not_found` when the task is not waiting.
    pub fn answer(&self, task_id: &str, choice: Choice, apply_to_batch: bool) -> Result<()> {
        let mut state = self.lock();
        let Some(waiting) = state.waiting.remove(task_id) else {
            return Err(Error::NotFound(format!(
                "task {task_id} is not waiting for an answer"
            )));
        };
        let group = waiting.pending.group_id.clone();
        let _ = waiting.answer.send(choice);
        state.emit(ConflictEvent::Resolved {
            task_id: task_id.to_owned(),
        });
        if apply_to_batch && let Some(group) = group {
            let siblings: Vec<String> = state
                .waiting
                .iter()
                .filter(|(_, other)| other.pending.group_id.as_deref() == Some(group.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            for id in siblings {
                if let Some(other) = state.waiting.remove(&id) {
                    let _ = other.answer.send(choice);
                    state.emit(ConflictEvent::Resolved { task_id: id });
                }
            }
            state.remembered.retain(|(known, _)| *known != group);
            if state.remembered.len() >= MAX_REMEMBERED {
                state.remembered.remove(0);
            }
            state.remembered.push((group, choice));
        }
        Ok(())
    }

    /// Forgets the "apply to all" answer of `group`.
    pub fn forget_group(&self, group: &str) {
        self.lock().remembered.retain(|(known, _)| known != group);
    }

    /// The resolver for the engines of one task.
    pub fn resolver_for(&self, task_id: &str, group_id: Option<&str>) -> Arc<dyn ConflictResolver> {
        Arc::new(TaskResolver {
            broker: self.clone(),
            task_id: task_id.to_owned(),
            group_id: group_id.map(str::to_owned),
        })
    }
}

struct TaskResolver {
    broker: ConflictBroker,
    task_id: String,
    group_id: Option<String>,
}

/// Removes a question that is dropped unanswered (the task was stopped).
struct Ticket<'a> {
    broker: &'a ConflictBroker,
    task_id: &'a str,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut state = self.broker.lock();
        if state.waiting.remove(self.task_id).is_some() {
            state.emit(ConflictEvent::Resolved {
                task_id: self.task_id.to_owned(),
            });
        }
    }
}

impl ConflictResolver for TaskResolver {
    fn resolve<'a>(&'a self, conflict: &'a Conflict) -> BoxFuture<'a, Result<Choice>> {
        Box::pin(async move {
            let receiver = {
                let mut state = self.broker.lock();
                if let Some(group) = &self.group_id
                    && let Some(choice) = state.remembered(group)
                {
                    return Ok(choice);
                }
                let (sender, receiver) = oneshot::channel();
                let pending = PendingConflict {
                    task_id: self.task_id.clone(),
                    group_id: self.group_id.clone(),
                    conflict: conflict.clone(),
                };
                state.waiting.insert(
                    self.task_id.clone(),
                    Waiting {
                        pending: pending.clone(),
                        answer: sender,
                    },
                );
                state.emit(ConflictEvent::Asked(pending));
                receiver
            };
            let _ticket = Ticket {
                broker: &self.broker,
                task_id: &self.task_id,
            };
            receiver.await.map_err(|_| Error::Cancelled)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::policy::ItemInfo;
    use crate::model::TransferKind;

    fn conflict(destination: &str) -> Conflict {
        Conflict {
            kind: TransferKind::Upload,
            source: "/tmp/a".into(),
            destination: destination.into(),
            source_info: ItemInfo::default(),
            destination_info: ItemInfo::default(),
        }
    }

    #[test]
    fn options_ride_in_the_group_id() {
        assert_eq!(JobOptions::default().group_id("g".into()), "g");
        let options = JobOptions {
            if_exists: Some(IfExists::OverwriteIfNewer),
            preserve_mtime: Some(false),
            dst_account: Some("acct-1".into()),
        };
        let group = options.group_id("1234-abcd".into());
        assert_eq!(group, "1234-abcd~ie=overwrite_if_newer;mt=0;da=acct-1");
        assert_eq!(JobOptions::of_group(Some(&group)), options);
        assert_eq!(JobOptions::of_group(Some("plain")), JobOptions::default());
        assert_eq!(JobOptions::of_group(None), JobOptions::default());
        let bad = JobOptions {
            dst_account: Some("a~b".into()),
            ..JobOptions::default()
        };
        assert!(bad.validate().is_err());
    }

    #[tokio::test]
    async fn an_answer_reaches_the_waiting_task_and_a_batch_answer_covers_its_siblings() {
        let broker = ConflictBroker::new();
        let mut events = broker.subscribe();
        let first = broker.resolver_for("t1", Some("g"));
        let second = broker.resolver_for("t2", Some("g"));
        let other = broker.resolver_for("t3", Some("h"));
        let (c1, c2, c3) = (conflict("a"), conflict("b"), conflict("c"));
        let wait1 = tokio::spawn(async move { first.resolve(&c1).await });
        let wait2 = tokio::spawn(async move { second.resolve(&c2).await });
        let wait3 = tokio::spawn(async move { other.resolve(&c3).await });
        while broker.pending().len() < 3 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(events.recv().await, Some(ConflictEvent::Asked(_))));
        broker.answer("t1", Choice::Skip, true).unwrap();
        assert_eq!(wait1.await.unwrap().unwrap(), Choice::Skip);
        assert_eq!(wait2.await.unwrap().unwrap(), Choice::Skip);
        assert_eq!(broker.pending().len(), 1);
        // A later question of the batch is settled without asking.
        let late = broker.resolver_for("t4", Some("g"));
        assert_eq!(late.resolve(&conflict("d")).await.unwrap(), Choice::Skip);
        broker.answer("t3", Choice::Rename, false).unwrap();
        assert_eq!(wait3.await.unwrap().unwrap(), Choice::Rename);
        assert!(broker.answer("t3", Choice::Skip, false).is_err());
    }

    #[tokio::test]
    async fn a_dropped_question_disappears() {
        let broker = ConflictBroker::new();
        let resolver = broker.resolver_for("t1", None);
        let question = conflict("a");
        let waiting = tokio::spawn(async move { resolver.resolve(&question).await });
        while broker.pending().is_empty() {
            tokio::task::yield_now().await;
        }
        waiting.abort();
        let _ = waiting.await;
        assert!(broker.pending().is_empty());
    }
}
