use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, Mutex, OwnedMutexGuard};

const EVENT_CAPACITY: usize = 256;
const REPLAY_CAPACITY: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigurationScope {
    Global {
        section: String,
    },
    Device {
        id: String,
    },
    Feature {
        device_id: String,
        feature_id: String,
    },
    Options {
        resource_id: String,
    },
}

impl ConfigurationScope {
    fn key(&self) -> String {
        match self {
            Self::Global { section } => format!("global:{section}"),
            Self::Device { id } => format!("device:{id}"),
            Self::Feature {
                device_id,
                feature_id,
            } => format!("feature:{device_id}:{feature_id}"),
            Self::Options { resource_id } => format!("options:{resource_id}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationChange {
    pub scope: ConfigurationScope,
    pub epoch: String,
    pub revision: u64,
    pub mutation_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ReplayRecord {
    payload_digest: String,
    result: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationReplayError {
    MutationIdReusedWithDifferentPayload,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigurationReplayResult {
    Miss,
    Committed(Value),
}

#[derive(Default)]
struct ReplayCache {
    order: VecDeque<(String, String, String)>,
    records: HashMap<(String, String, String), ReplayRecord>,
}

#[derive(Clone, Debug)]
struct OptionObservation {
    digest: String,
    revision: u64,
}

pub struct ConfigurationCoordinator {
    transaction: Arc<Mutex<()>>,
    epoch: String,
    revision: AtomicU64,
    scope_revisions: StdMutex<HashMap<String, u64>>,
    publication: StdMutex<()>,
    events: broadcast::Sender<ConfigurationChange>,
    replay: Mutex<ReplayCache>,
    option_observations: Mutex<HashMap<String, OptionObservation>>,
}

impl Default for ConfigurationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigurationCoordinator {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            transaction: Arc::new(Mutex::new(())),
            epoch: uuid::Uuid::new_v4().to_string(),
            revision: AtomicU64::new(0),
            scope_revisions: StdMutex::new(HashMap::new()),
            publication: StdMutex::new(()),
            events,
            replay: Mutex::new(ReplayCache::default()),
            option_observations: Mutex::new(HashMap::new()),
        }
    }

    pub async fn lock(&self) -> OwnedMutexGuard<()> {
        self.transaction.clone().lock_owned().await
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConfigurationChange> {
        self.events.subscribe()
    }

    pub fn etag<T: Serialize + ?Sized>(
        &self,
        scope: &ConfigurationScope,
        public_snapshot: &T,
    ) -> Result<String, String> {
        let scope_key = scope.key();
        let scope_revision = self
            .scope_revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&scope_key)
            .copied()
            .unwrap_or(0);
        crate::bmc::configuration::configuration_etag(&serde_json::json!({
            "epoch": self.epoch,
            "scope": scope,
            "scope_revision": scope_revision,
            "snapshot": public_snapshot,
        }))
    }

    pub fn advance_scope(&self, scope: &ConfigurationScope) {
        let mut revisions = self
            .scope_revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *revisions.entry(scope.key()).or_insert(0) += 1;
    }

    pub fn publish(
        &self,
        scope: ConfigurationScope,
        mutation_id: impl Into<String>,
        changed_paths: Vec<String>,
        affected_features: Vec<String>,
    ) -> ConfigurationChange {
        let mutation_id = mutation_id.into();
        // Revision assignment and enqueue form one publication. Otherwise a
        // newer event can arrive first and make clients discard an older scope's change.
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let change = ConfigurationChange {
            scope,
            epoch: self.epoch.clone(),
            revision: self.revision.fetch_add(1, Ordering::SeqCst) + 1,
            mutation_id,
            changed_paths,
            affected_features,
        };
        let _ = self.events.send(change.clone());
        change
    }

    pub async fn replay(
        &self,
        principal: &str,
        scope: &ConfigurationScope,
        mutation_id: &str,
        payload_digest: &str,
    ) -> Result<ConfigurationReplayResult, ConfigurationReplayError> {
        let key = (principal.to_string(), scope.key(), mutation_id.to_string());
        let replay = self.replay.lock().await;
        let Some(record) = replay.records.get(&key) else {
            return Ok(ConfigurationReplayResult::Miss);
        };
        if record.payload_digest != payload_digest {
            return Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload);
        }
        Ok(ConfigurationReplayResult::Committed(record.result.clone()))
    }

    pub async fn remember(
        &self,
        principal: &str,
        scope: &ConfigurationScope,
        mutation_id: &str,
        payload_digest: String,
        result: Value,
    ) {
        let key = (principal.to_string(), scope.key(), mutation_id.to_string());
        let record = ReplayRecord {
            payload_digest,
            result,
        };
        let mut replay = self.replay.lock().await;
        if replay.records.insert(key.clone(), record).is_none() {
            replay.order.push_back(key);
        }
        while replay.order.len() > REPLAY_CAPACITY {
            if let Some(expired) = replay.order.pop_front() {
                replay.records.remove(&expired);
            }
        }
    }

    pub async fn observe_options(&self, resource_id: &str, digest: String) -> (u64, bool) {
        let mut observations = self.option_observations.lock().await;
        match observations.get_mut(resource_id) {
            Some(observation) if observation.digest == digest => (observation.revision, false),
            Some(observation) => {
                observation.digest = digest;
                observation.revision += 1;
                (observation.revision, true)
            }
            None => {
                observations.insert(
                    resource_id.to_string(),
                    OptionObservation {
                        digest,
                        revision: 1,
                    },
                );
                (1, false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_scope() -> ConfigurationScope {
        ConfigurationScope::Device {
            id: "device-1".to_string(),
        }
    }

    #[tokio::test]
    async fn committed_mutation_replays_only_for_the_same_payload() {
        let coordinator = ConfigurationCoordinator::new();
        coordinator
            .remember(
                "principal",
                &device_scope(),
                "mutation-1",
                "digest-1".to_string(),
                serde_json::json!({"success": true}),
            )
            .await;

        assert_eq!(
            coordinator
                .replay("principal", &device_scope(), "mutation-1", "digest-1")
                .await,
            Ok(ConfigurationReplayResult::Committed(
                serde_json::json!({"success": true})
            ))
        );
        assert_eq!(
            coordinator
                .replay("principal", &device_scope(), "mutation-1", "different")
                .await,
            Err(ConfigurationReplayError::MutationIdReusedWithDifferentPayload)
        );
        assert_eq!(
            coordinator
                .replay("other-principal", &device_scope(), "mutation-1", "digest-1")
                .await,
            Ok(ConfigurationReplayResult::Miss)
        );
    }

    #[test]
    fn delayed_publication_cannot_broadcast_an_older_revision_after_a_newer_one() {
        struct DelayedId(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        );
        impl From<DelayedId> for String {
            fn from(id: DelayedId) -> Self {
                id.0.send(()).unwrap();
                id.1.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                "delayed".into()
            }
        }
        let coordinator = Arc::new(ConfigurationCoordinator::new());
        let mut events = coordinator.subscribe();
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let first = std::thread::spawn({
            let coordinator = coordinator.clone();
            move || coordinator.publish(device_scope(), DelayedId(started, wait), vec![], vec![])
        });
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        coordinator.publish(device_scope(), "immediate", vec![], vec![]);
        release.send(()).unwrap();
        first.join().unwrap();
        assert_eq!(
            [
                events.try_recv().unwrap().revision,
                events.try_recv().unwrap().revision
            ],
            [1, 2]
        );
    }

    #[test]
    fn concurrent_configuration_publications_reach_subscribers_in_revision_order() {
        const WRITERS: usize = 8;
        const CHANGES: usize = 128;
        let mut coordinator = ConfigurationCoordinator::new();
        let (events, mut receiver) = broadcast::channel(WRITERS * CHANGES);
        coordinator.events = events;
        let barrier = std::sync::Barrier::new(WRITERS);
        std::thread::scope(|threads| {
            for writer in 0..WRITERS {
                let coordinator = &coordinator;
                let barrier = &barrier;
                threads.spawn(move || {
                    barrier.wait();
                    for index in 0..CHANGES {
                        coordinator.publish(
                            device_scope(),
                            format!("{writer}-{index}"),
                            vec![],
                            vec![],
                        );
                    }
                });
            }
        });
        for revision in 1..=(WRITERS * CHANGES) as u64 {
            assert_eq!(receiver.try_recv().unwrap().revision, revision);
        }
        assert_eq!(coordinator.current_revision(), (WRITERS * CHANGES) as u64);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn published_changes_are_strictly_revisioned() {
        let coordinator = ConfigurationCoordinator::new();
        let mut events = coordinator.subscribe();

        let first = coordinator.publish(device_scope(), "mutation-1", Vec::new(), Vec::new());
        let second = coordinator.publish(device_scope(), "mutation-2", Vec::new(), Vec::new());

        assert_eq!(first.revision, 1);
        assert_eq!(second.revision, 2);
        assert_eq!(first.epoch, second.epoch);
        assert_eq!(events.recv().await.unwrap(), first);
        assert_eq!(events.recv().await.unwrap(), second);
    }

    #[test]
    fn etag_changes_only_when_its_scope_is_published() {
        let coordinator = ConfigurationCoordinator::new();
        let device = device_scope();
        let other = ConfigurationScope::Device {
            id: "device-2".to_string(),
        };
        let public_snapshot = serde_json::json!({"has_password": true});
        let first = coordinator.etag(&device, &public_snapshot).unwrap();

        coordinator.publish(other, "other", Vec::new(), Vec::new());
        assert_eq!(coordinator.etag(&device, &public_snapshot).unwrap(), first);

        coordinator.advance_scope(&device);
        coordinator.publish(device.clone(), "device", Vec::new(), Vec::new());
        assert_ne!(coordinator.etag(&device, &public_snapshot).unwrap(), first);
    }

    #[tokio::test]
    async fn option_revisions_advance_only_when_the_snapshot_changes() {
        let coordinator = ConfigurationCoordinator::new();

        assert_eq!(
            coordinator.observe_options("video", "a".into()).await,
            (1, false)
        );
        assert_eq!(
            coordinator.observe_options("video", "a".into()).await,
            (1, false)
        );
        assert_eq!(
            coordinator.observe_options("video", "b".into()).await,
            (2, true)
        );
        assert_eq!(
            coordinator.observe_options("serial", "a".into()).await,
            (1, false)
        );
    }
}
