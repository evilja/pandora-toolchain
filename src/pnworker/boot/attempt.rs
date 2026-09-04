// One run of one profile against one node, and what it left behind. Attempts are persisted because
// the interesting state is not "this boot succeeded" but "a provider request was sent and nobody
// heard the answer": a process that restarts mid-attempt and forgets it would issue the same
// create-instance POST again, and the second machine is one nobody asked for and everybody pays
// for.
//
// The record is deliberately small and bounded. It holds what a retry needs to be safe — the
// provider's own operation identifier, the step reached, the outcome — and nothing that grows.

use serde::{Deserialize, Serialize};

use crate::lib::env::standard::LINK_BOOT_ATTEMPTS_PATH;
use crate::lib::sync::lock;
use std::sync::{Mutex, OnceLock};

// Kept per node, newest last. Enough to see a pattern on `/lsnode` without the file becoming a log.
const MAX_HISTORY_PER_NODE: usize = 5;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BootStatus {
    // A provider request is in flight or the sequence is between steps.
    Booting { step: usize, step_id: String },
    // Every request succeeded. The machine is starting but has not said hello yet, so the scheduler
    // still cannot use it — provider success is not readiness.
    WaitingForRegistration,
    // The node registered and was accepted.
    Ready,
    Failed { reason: String },
    // A request was sent and its outcome is unknown: a timeout, or a crash between sending and
    // recording. This never retries by itself. Renting again on a guess is the expensive mistake,
    // so the attempt stops here and says what to reconcile.
    OutcomeUnknown { reason: String },
}

impl BootStatus {
    pub fn label(&self) -> String {
        match self {
            BootStatus::Booting { step, step_id } => format!("booting (step {step}, `{step_id}`)"),
            BootStatus::WaitingForRegistration => "waiting for registration".to_string(),
            BootStatus::Ready => "ready".to_string(),
            BootStatus::Failed { reason } => format!("failed: {reason}"),
            BootStatus::OutcomeUnknown { reason } => format!("outcome unknown: {reason}"),
        }
    }

    // Whether this attempt still occupies its node and counts against the concurrency limit.
    pub fn in_flight(&self) -> bool {
        matches!(
            self,
            BootStatus::Booting { .. } | BootStatus::WaitingForRegistration
        )
    }

    // Whether this attempt blocks any further boot of the node until a human acts. An unknown
    // outcome does: it is the state that exists precisely because retrying is not safe.
    pub fn blocks_retry(&self) -> bool {
        matches!(self, BootStatus::OutcomeUnknown { .. })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BootAttempt {
    pub id: String,
    pub node: String,
    pub profile: String,
    pub profile_revision: u64,
    pub status: BootStatus,
    pub started_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    // The provider's own identifier for whatever this attempt created, recorded the moment it is
    // captured. Without it an unknown outcome cannot be reconciled against anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_operation: Option<String>,
    // Not sent to any provider by default; it is offered to profiles as
    // `${attempt.idempotency_key}` so one that supports the header can make its own retry safe.
    pub idempotency_key: String,
    // When this node may be booted again, for a cooldown after a failure.
    #[serde(default)]
    pub cooldown_until: u64,
    // Consecutive failed attempts, for the profile's `max_attempts`. Reset by a success.
    #[serde(default)]
    pub consecutive_failures: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct AttemptFile {
    #[serde(default)]
    attempts: Vec<BootAttempt>,
}

fn store() -> &'static Mutex<Option<AttemptFile>> {
    static STORE: OnceLock<Mutex<Option<AttemptFile>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

fn read_file() -> AttemptFile {
    let Ok(contents) = std::fs::read_to_string(LINK_BOOT_ATTEMPTS_PATH) else {
        return AttemptFile::default();
    };
    serde_json::from_str(&contents).unwrap_or_else(|error| {
        eprintln!("[boot] attempt records are unreadable ({error}); starting empty");
        AttemptFile::default()
    })
}

fn save(file: &AttemptFile) {
    let Ok(body) = serde_json::to_string_pretty(file) else {
        return;
    };
    let path = std::path::Path::new(LINK_BOOT_ATTEMPTS_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, body).is_err() {
        return;
    }
    if std::fs::rename(&temporary, path).is_err() {
        std::fs::remove_file(&temporary).ok();
    }
}

fn with<R>(f: impl FnOnce(&mut AttemptFile) -> R) -> R {
    let mut guard = lock(store());
    if guard.is_none() {
        *guard = Some(read_file());
    }
    let file = guard.as_mut().expect("filled above");
    let out = f(file);
    let snapshot = file.clone();
    drop(guard);
    save(&snapshot);
    out
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn all() -> Vec<BootAttempt> {
    let mut guard = lock(store());
    if guard.is_none() {
        *guard = Some(read_file());
    }
    guard.as_ref().map(|f| f.attempts.clone()).unwrap_or_default()
}

// The newest attempt for a node, which is the one every decision is made against.
pub fn latest(node: &str) -> Option<BootAttempt> {
    all().into_iter().filter(|a| a.node == node).next_back()
}

pub fn in_flight_count() -> usize {
    all().iter().filter(|a| a.status.in_flight()).count()
}

// Recovers attempts left in flight by a process that stopped. An attempt that was mid-request when
// the coordinator died has an outcome nobody observed, and that is exactly the case that must not
// be retried automatically — so it is moved to `OutcomeUnknown` rather than cleared. One that had
// finished its requests and was only waiting for the node to register is safe to reopen as demand.
pub fn reconcile_on_start() {
    let mut changed = Vec::new();
    with(|file| {
        for attempt in file.attempts.iter_mut() {
            match &attempt.status {
                BootStatus::Booting { step, step_id } => {
                    let reason = format!(
                        "the coordinator stopped during step {step} (`{step_id}`); reconcile {} with the provider before this node is booted again",
                        attempt
                            .provider_operation
                            .as_deref()
                            .map(|op| format!("operation `{op}`"))
                            .unwrap_or_else(|| "the attempt".to_string())
                    );
                    attempt.status = BootStatus::OutcomeUnknown { reason };
                    attempt.updated_at = now();
                    changed.push(attempt.node.clone());
                }
                // Every request had already been answered. Nothing is owed to the provider and the
                // machine either arrives or does not, so this simply expires.
                BootStatus::WaitingForRegistration => {
                    attempt.status = BootStatus::Failed {
                        reason: "the coordinator restarted before this node registered".to_string(),
                    };
                    attempt.updated_at = now();
                }
                _ => {}
            }
        }
    });
    for node in changed {
        eprintln!("[boot] {node} | an attempt was interrupted; its outcome is unknown and automatic boot is held");
    }
}

pub fn begin(
    node: &str,
    profile: &str,
    profile_revision: u64,
    idempotency_key: String,
) -> BootAttempt {
    let previous = latest(node);
    let attempt = BootAttempt {
        id: format!("{}-{}", now(), &idempotency_key[..8.min(idempotency_key.len())]),
        node: node.to_string(),
        profile: profile.to_string(),
        profile_revision,
        status: BootStatus::Booting {
            step: 1,
            step_id: String::new(),
        },
        started_at: now(),
        updated_at: now(),
        provider_operation: None,
        idempotency_key,
        cooldown_until: 0,
        consecutive_failures: previous.map(|p| p.consecutive_failures).unwrap_or(0),
    };
    with(|file| {
        file.attempts.push(attempt.clone());
        prune(file, node);
    });
    attempt
}

fn prune(file: &mut AttemptFile, node: &str) {
    let mut indices: Vec<usize> = file
        .attempts
        .iter()
        .enumerate()
        .filter(|(_, a)| a.node == node)
        .map(|(i, _)| i)
        .collect();
    while indices.len() > MAX_HISTORY_PER_NODE {
        let oldest = indices.remove(0);
        file.attempts.remove(oldest);
        indices.iter_mut().for_each(|i| *i -= 1);
    }
}

fn update(id: &str, f: impl FnOnce(&mut BootAttempt)) {
    with(|file| {
        if let Some(attempt) = file.attempts.iter_mut().find(|a| a.id == id) {
            f(attempt);
            attempt.updated_at = now();
        }
    });
}

pub fn set_step(id: &str, step: usize, step_id: &str) {
    update(id, |attempt| {
        attempt.status = BootStatus::Booting {
            step,
            step_id: step_id.to_string(),
        };
    });
}

// Recorded the moment it is captured rather than at the end of the sequence: an attempt that dies
// on the next step still has to be reconcilable, and the identifier is the only thing that makes
// that possible.
pub fn set_provider_operation(id: &str, operation: &str) {
    update(id, |attempt| {
        attempt.provider_operation = Some(operation.to_string());
    });
}

pub fn set_waiting_for_registration(id: &str) {
    update(id, |attempt| {
        attempt.status = BootStatus::WaitingForRegistration;
    });
}

pub fn set_ready(id: &str) {
    update(id, |attempt| {
        attempt.status = BootStatus::Ready;
        attempt.consecutive_failures = 0;
        attempt.cooldown_until = 0;
    });
}

pub fn set_failed(id: &str, reason: &str, cooldown_secs: u64) {
    update(id, |attempt| {
        attempt.status = BootStatus::Failed {
            reason: reason.to_string(),
        };
        attempt.consecutive_failures = attempt.consecutive_failures.saturating_add(1);
        attempt.cooldown_until = now().saturating_add(cooldown_secs);
    });
}

pub fn set_outcome_unknown(id: &str, reason: &str) {
    update(id, |attempt| {
        attempt.status = BootStatus::OutcomeUnknown {
            reason: reason.to_string(),
        };
        attempt.consecutive_failures = attempt.consecutive_failures.saturating_add(1);
    });
}

// Called when a node registers. It is what turns "the provider said yes" into "the scheduler may
// use this machine", and it is the only thing that does — a boot is not finished by a 200.
pub fn note_registration(node: &str) {
    let Some(attempt) = latest(node) else {
        return;
    };
    if !attempt.status.in_flight() {
        return;
    }
    set_ready(&attempt.id);
    println!("[boot] {node} | registered; the boot attempt is complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_in_flight_status_holds_a_node_and_a_terminal_one_does_not() {
        assert!(BootStatus::WaitingForRegistration.in_flight());
        assert!(
            BootStatus::Booting {
                step: 1,
                step_id: "start".into()
            }
            .in_flight()
        );
        assert!(!BootStatus::Ready.in_flight());
        assert!(
            !BootStatus::Failed {
                reason: "x".into()
            }
            .in_flight()
        );
    }

    #[test]
    fn only_an_unknown_outcome_blocks_a_retry() {
        assert!(
            BootStatus::OutcomeUnknown {
                reason: "timeout".into()
            }
            .blocks_retry()
        );
        assert!(
            !BootStatus::Failed {
                reason: "403".into()
            }
            .blocks_retry()
        );
        assert!(!BootStatus::Ready.blocks_retry());
    }

    #[test]
    fn a_status_label_names_the_step_it_stopped_on() {
        let label = BootStatus::Booting {
            step: 2,
            step_id: "inspect".into(),
        }
        .label();
        assert!(label.contains('2') && label.contains("inspect"), "{label}");
    }

    #[test]
    fn pruning_keeps_the_newest_attempts_for_one_node_and_leaves_others_alone() {
        let mut file = AttemptFile::default();
        for i in 0..(MAX_HISTORY_PER_NODE + 3) {
            file.attempts.push(BootAttempt {
                id: format!("a{i}"),
                node: "n".into(),
                profile: "p".into(),
                profile_revision: 0,
                status: BootStatus::Ready,
                started_at: i as u64,
                updated_at: 0,
                provider_operation: None,
                idempotency_key: "k".into(),
                cooldown_until: 0,
                consecutive_failures: 0,
            });
        }
        file.attempts.push(BootAttempt {
            id: "other".into(),
            node: "m".into(),
            profile: "p".into(),
            profile_revision: 0,
            status: BootStatus::Ready,
            started_at: 0,
            updated_at: 0,
            provider_operation: None,
            idempotency_key: "k".into(),
            cooldown_until: 0,
            consecutive_failures: 0,
        });
        prune(&mut file, "n");
        let kept: Vec<&str> = file
            .attempts
            .iter()
            .filter(|a| a.node == "n")
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(kept.len(), MAX_HISTORY_PER_NODE);
        assert_eq!(kept.last(), Some(&"a7"));
        assert!(file.attempts.iter().any(|a| a.id == "other"));
    }
}
