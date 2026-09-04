// Decides when a machine that is not running should be started, and runs the profile that starts
// it. This is a task of its own rather than part of `pn_worker`: a boot is a sequence of HTTP
// requests that can take minutes, and the queue loop runs twenty times a second and holds the
// roster lock while it schedules.
//
// The trigger is unmet queued demand and nothing else. A node going offline is not a reason to rent
// a machine — a cluster that quietly re-rents a box every time one reboots is a bill, not a feature
// — so what this reads is the set of jobs that are *waiting for a node right now*, which on an
// orchestrator is exactly the jobs that cannot run anywhere. On a coordinator that encodes, a job
// that finds no node runs locally instead and never appears here; the local fallback is preserved,
// so boot profiles only ever change what a deployment does when a job would otherwise wait.

use std::collections::BTreeMap;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    LINK_BOOT_ENABLED, LINK_BOOT_MAX_CONCURRENT, LINK_COORDINATOR_URL,
};
use crate::lib::mpeg::preset;
use crate::lib::sync::lock;
use std::sync::{Mutex, OnceLock};

use super::attempt::{self, BootStatus};
use super::binding::{self, BootBinding};
use super::exec::{BootContext, StepOutcome};
use super::profile;

const DEFAULT_MAX_CONCURRENT: usize = 1;
// How often demand is reconciled. Slow on purpose: the answer changes when a job is submitted or a
// node registers, both of which are human-scale events, and every pass reads the queue snapshot and
// the roster.
const RECONCILE_SECS: u64 = 5;
// A node that has just registered still has to be accepted and offered work. Without this, a boot
// whose machine is mid-registration would look like unmet demand for one more pass and start a
// second attempt.
const STARTING_GRACE_SECS: u64 = 900;

// One waiting job, in the only terms a boot decision needs. Published by the worker loop rather
// than pulled from it, so this task never touches the queue or takes the roster lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demand {
    pub job_id: u64,
    pub preset: String,
    pub server: Option<u64>,
}

fn demand_store() -> &'static Mutex<Vec<Demand>> {
    static DEMAND: OnceLock<Mutex<Vec<Demand>>> = OnceLock::new();
    DEMAND.get_or_init(|| Mutex::new(Vec::new()))
}

// Called from `pn_worker` once a pass. Cheap: it replaces a small vector behind an uncontended
// mutex, and the loop is the only writer.
pub fn publish_demand(demand: Vec<Demand>) {
    *lock(demand_store()) = demand;
}

pub fn current_demand() -> Vec<Demand> {
    lock(demand_store()).clone()
}

#[derive(Clone, Debug)]
pub struct BootSettings {
    pub enabled: bool,
    pub max_concurrent: usize,
}

pub fn settings() -> BootSettings {
    let env = get_pandora_env();
    let enabled = matches!(
        env.get(LINK_BOOT_ENABLED)
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("true") | Some("1") | Some("yes") | Some("on")
    );
    BootSettings {
        enabled,
        max_concurrent: env
            .get(LINK_BOOT_MAX_CONCURRENT)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_CONCURRENT),
    }
}

// Why a node that has a binding is not a candidate for this pass. Every one of these is shown
// somewhere an operator looks, because a boot system whose answer to "why is nothing happening" is
// silence is worse than no boot system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ineligible {
    NoToken,
    ProfileMissing(String),
    ProfileDisabled,
    CapabilityMismatch(String),
    AlreadyOnline,
    AlreadyBooting,
    OutcomeUnknown(String),
    CoolingDown(u64),
    AttemptsExhausted(u32),
    Drained,
}

impl Ineligible {
    pub fn describe(&self) -> String {
        match self {
            Ineligible::NoToken => {
                "no link token names this node, so nothing authorises booting it".to_string()
            }
            Ineligible::ProfileMissing(e) => format!("its boot profile could not be read — {e}"),
            Ineligible::ProfileDisabled => "its boot profile is disabled".to_string(),
            Ineligible::CapabilityMismatch(reason) => reason.clone(),
            Ineligible::AlreadyOnline => "it is already registered".to_string(),
            Ineligible::AlreadyBooting => "a boot attempt is already running".to_string(),
            Ineligible::OutcomeUnknown(reason) => {
                format!("its last attempt ended with an unknown outcome — {reason}")
            }
            Ineligible::CoolingDown(secs) => format!("it is cooling down for another {secs}s"),
            Ineligible::AttemptsExhausted(n) => {
                format!("{n} attempts in a row failed; edit the profile to clear it")
            }
            Ineligible::Drained => "it is drained".to_string(),
        }
    }
}

// The state of one binding for the operator views. Separate from `Ineligible` because a node that
// is perfectly fine and simply not needed is not a problem to report.
pub struct BootState {
    pub binding: BootBinding,
    pub status: Option<BootStatus>,
    pub blocked: Option<Ineligible>,
}

// Everything `/lsnode` shows about boot. Built from the same predicates the scheduler uses, so what
// an operator reads is what the manager decided and not a second opinion.
pub fn states() -> Vec<BootState> {
    let tokens = binding::nodes_with_tokens();
    let online = online_nodes();
    binding::all()
        .into_iter()
        .map(|binding| {
            let status = attempt::latest(&binding.node).map(|a| a.status);
            let blocked = eligibility(&binding, &tokens, &online).err();
            BootState {
                binding,
                status,
                blocked,
            }
        })
        .collect()
}

// The last view this task published. `pn_worker` builds the worker snapshot once a second and
// reads this rather than building it, because building it opens the token file and every bound
// profile — blocking work, on the loop that owns the whole queue. The manager is already awake on
// its own cadence and is the right place for those reads.
fn published_view() -> &'static Mutex<Option<serde_json::Value>> {
    static VIEW: OnceLock<Mutex<Option<serde_json::Value>>> = OnceLock::new();
    VIEW.get_or_init(|| Mutex::new(None))
}

// Read by the worker loop. An empty array until the manager has published once, which is the
// truthful answer: nothing is known about boot state yet.
pub fn boot_view() -> serde_json::Value {
    lock(published_view())
        .clone()
        .unwrap_or_else(|| serde_json::json!([]))
}

// The boot section of the worker snapshot, which is what `/workers` and `GET /api/v1/workers` read.
// Same source as `/lsnode`, so the console and Discord cannot disagree about why a node is stuck.
pub fn build_view() -> serde_json::Value {
    serde_json::json!(
        states()
            .into_iter()
            .map(|state| serde_json::json!({
                "node": state.binding.node,
                "profile": state.binding.profile,
                "profile_revision": state.binding.profile_revision,
                "purpose": state.binding.purpose.label(),
                "expected_encoders": state.binding.expected_encoders,
                "image_revision": state.binding.image_revision,
                // What the machine actually proved, against the image revision it proved it under.
                // Both, because a proof taken on hardware the profile no longer rents is not one.
                "proven_encoders": state.binding.proven_encoders,
                "proven_image_revision": state.binding.proven_image_revision,
                "status": state.status.as_ref().map(|s| s.label()),
                "blocked": state.blocked.as_ref().map(|b| b.describe()),
            }))
            .collect::<Vec<_>>()
    )
}

fn online_nodes() -> std::collections::HashMap<String, bool> {
    let settings = crate::pnworker::link::board::settings();
    crate::pnworker::link::board::roster()
        .into_iter()
        .map(|(node, _)| {
            let alive = crate::pnworker::link::board::is_alive(&node, settings.lease_timeout_secs);
            (node.name, alive && !node.drain)
        })
        .collect()
}

// Whether this binding could be booted at all right now, ignoring whether anything needs it. Split
// from demand matching so the reason a node is unavailable is answerable without a queue.
fn eligibility(
    binding: &BootBinding,
    tokens: &std::collections::HashSet<String>,
    online: &std::collections::HashMap<String, bool>,
) -> Result<profile::BootProfile, Ineligible> {
    if !tokens.contains(&binding.node) {
        return Err(Ineligible::NoToken);
    }
    // A registered, healthy node needs no boot. A registered node that is *drained* needs no boot
    // either, and must not get one: draining is an operator saying "leave this machine alone".
    match online.get(&binding.node) {
        Some(true) => return Err(Ineligible::AlreadyOnline),
        Some(false) => return Err(Ineligible::Drained),
        None => {}
    }
    let loaded =
        profile::load(&binding.profile).map_err(|e| Ineligible::ProfileMissing(shorten(&e)))?;
    if !loaded.file.enabled {
        return Err(Ineligible::ProfileDisabled);
    }
    // Checked against the *current* profile revision: a mismatch is a fact about the hardware the
    // profile rented, and editing the profile is how an operator says the plan has changed. Without
    // the comparison the only way out of a suppressed binding would be to hand-edit a state file.
    if let Some(reason) = &binding.capability_mismatch {
        if binding.profile_revision == loaded.revision {
            return Err(Ineligible::CapabilityMismatch(reason.clone()));
        }
    }
    if let Some(last) = attempt::latest(&binding.node) {
        match &last.status {
            BootStatus::Booting { .. } | BootStatus::WaitingForRegistration => {
                // A machine that finished its requests but never arrived should not hold the node
                // for ever; past the grace window the attempt is stale and a new one may run.
                if attempt::now().saturating_sub(last.updated_at) < STARTING_GRACE_SECS {
                    return Err(Ineligible::AlreadyBooting);
                }
            }
            BootStatus::OutcomeUnknown { reason } => {
                return Err(Ineligible::OutcomeUnknown(shorten(reason)));
            }
            _ => {}
        }
        let now = attempt::now();
        if last.cooldown_until > now {
            return Err(Ineligible::CoolingDown(last.cooldown_until - now));
        }
        // A profile edited since the failures is a new plan, so the count starts again. Without
        // this the only way out of an exhausted binding would be to hand-edit a state file.
        if last.consecutive_failures >= loaded.file.max_attempts
            && last.profile_revision == loaded.revision
        {
            return Err(Ineligible::AttemptsExhausted(last.consecutive_failures));
        }
    }
    Ok(loaded)
}

fn shorten(text: &str) -> String {
    let one_line = text.replace(['\n', '\r'], " ");
    if one_line.chars().count() > 180 {
        format!("{}…", one_line.chars().take(180).collect::<String>())
    } else {
        one_line
    }
}

// Capacity that already exists or is on its way, per the demand it could serve. Counting a machine
// that is mid-boot is what stops a queue of ten episodes renting ten machines for the one node that
// can run them.
fn starting_nodes() -> Vec<String> {
    attempt::all()
        .into_iter()
        .filter(|a| a.status.in_flight())
        .map(|a| a.node)
        .collect()
}

// What a preset needs of a machine, in the same two terms `pick_node` asks a registered node for.
fn requirement(preset_name: &str) -> (preset::PresetHardware, Option<String>) {
    let hardware = preset::hardware_for(preset_name);
    let required = (hardware == preset::PresetHardware::Gpu)
        .then(|| preset::video_codec_for(preset_name))
        .flatten();
    (hardware, required)
}

// Demand that no machine already on its way can serve. This is the rule that keeps a queue of ten
// episodes from renting ten machines: a node partway through its boot is capacity that exists, it
// simply has not arrived, and counting it is the difference between one instance and a bill.
//
// Note it deliberately does not divide demand by capacity. One booting machine covers every waiting
// job it could serve, because a node takes jobs one after another and the queue drains; renting a
// second box to shorten a queue is a decision about money that this is not entitled to make.
fn unmet_demand<'a>(
    demand: &'a [Demand],
    bindings: &[BootBinding],
    starting: &[String],
) -> Vec<&'a Demand> {
    demand
        .iter()
        .filter(|job| {
            let (hardware, required) = requirement(&job.preset);
            !starting.iter().any(|node| {
                bindings
                    .iter()
                    .find(|b| &b.node == node)
                    .is_some_and(|b| b.could_serve(hardware, required.as_deref()))
            })
        })
        .collect()
}

// Copies each profile's declared capabilities onto its binding. A binding took them at mint time,
// and a profile edited since is the operator saying what the machine is now — the scheduler matches
// demand against the binding, so without this an added encoder would never take effect.
fn refresh_expectations() {
    for binding in binding::all() {
        if let Ok(loaded) = profile::load(&binding.profile) {
            binding::refresh_expectations(&binding.node, loaded.revision, &loaded.file.capabilities);
        }
    }
}

// The pass. Returns the bindings it started, which is what the tests read.
pub async fn reconcile_once() -> Vec<String> {
    // Demand first, and it is a mutex and a clone. Everything below this line reads files, and a
    // coordinator with nothing waiting — which is a coordinator almost all of the time — should not
    // be opening `env.pandora` and a profile directory twelve times a minute to discover that.
    let demand = current_demand();
    if demand.is_empty() {
        return Vec::new();
    }
    let settings = settings();
    if !settings.enabled {
        return Vec::new();
    }
    let in_flight = attempt::in_flight_count();
    if in_flight >= settings.max_concurrent {
        return Vec::new();
    }

    refresh_expectations();
    let tokens = binding::nodes_with_tokens();
    let online = online_nodes();
    let starting = starting_nodes();
    let bindings = binding::all();

    let unmet = unmet_demand(&demand, &bindings, &starting);
    if unmet.is_empty() {
        return Vec::new();
    }

    let mut budget = settings.max_concurrent.saturating_sub(in_flight);
    let mut started = Vec::new();
    let mut claimed: Vec<&BootBinding> = Vec::new();
    for job in unmet {
        if budget == 0 {
            break;
        }
        let (hardware, required) = requirement(&job.preset);
        // One machine per pass per unmet job, and never one already claimed for an earlier job in
        // this same pass.
        let Some((candidate, loaded)) = bindings
            .iter()
            .filter(|b| !claimed.iter().any(|c| c.node == b.node))
            .filter(|b| b.could_serve(hardware, required.as_deref()))
            .find_map(|b| eligibility(b, &tokens, &online).ok().map(|p| (b, p)))
        else {
            continue;
        };
        claimed.push(candidate);
        budget -= 1;
        started.push(candidate.node.clone());
        spawn_attempt(candidate.clone(), loaded, job.job_id);
    }
    started
}

// Each attempt runs in its own task. The reconcile pass must not block behind a provider that takes
// four minutes to answer, and two nodes booting at once for different demand is the ordinary case.
fn spawn_attempt(binding: BootBinding, loaded: profile::BootProfile, for_job: u64) {
    tokio::spawn(async move {
        run_attempt(binding, loaded, for_job).await;
    });
}

async fn run_attempt(binding: BootBinding, loaded: profile::BootProfile, for_job: u64) {
    let node = binding.node.clone();
    let secrets = match super::secrets::load() {
        Ok(secrets) => secrets,
        Err(e) => {
            eprintln!("[boot] {node} | {e}");
            return;
        }
    };
    let Ok(key) = crate::lib::secret::random_hex_token() else {
        eprintln!("[boot] {node} | could not generate an idempotency key; not booting");
        return;
    };
    let record = attempt::begin(&node, &loaded.id, loaded.revision, key.clone());

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("node.name".into(), node.clone());
    vars.insert("node.purpose".into(), binding.purpose.label().to_string());
    vars.insert("attempt.id".into(), record.id.clone());
    vars.insert("attempt.idempotency_key".into(), key);
    // The machine has to be able to find its way home and prove who it is. The token is the node's
    // own link credential, never a provider key: a rented box is given exactly what it needs to
    // register and nothing that could spend money.
    let env = get_pandora_env();
    vars.insert(
        "node.coordinator_url".into(),
        env.get(LINK_COORDINATOR_URL)
            .or_else(|| env.get(crate::lib::env::standard::API_PUBLIC_URL))
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
    );
    vars.insert("node.token".into(), node_token(&node).unwrap_or_default());

    println!(
        "[boot] {node} | starting profile `{}` for job {for_job}",
        loaded.id
    );
    let mut ctx = BootContext {
        attempt_id: record.id.clone(),
        node: node.clone(),
        vars,
        secrets,
    };
    match super::exec::run(&loaded, &mut ctx).await {
        StepOutcome::Completed => {
            attempt::set_waiting_for_registration(&record.id);
            println!("[boot] {node} | every step succeeded; waiting for it to register");
        }
        StepOutcome::Failed(reason) => {
            attempt::set_failed(&record.id, &reason, loaded.file.cooldown_secs);
            eprintln!("[boot] {node} | {reason}");
        }
        StepOutcome::Unknown(reason) => {
            attempt::set_outcome_unknown(&record.id, &reason);
            eprintln!("[boot] {node} | {reason}");
        }
    }
}

// The node's own link token, read at execution time rather than stored in the binding. A revoked
// token must stop authorising a boot immediately, and a copy in the binding would outlive it.
fn node_token(node: &str) -> Option<String> {
    let contents = std::fs::read_to_string(crate::lib::env::standard::API_TOKENS_PATH).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
        if fields.len() >= 3 && fields[1] == "link" && fields[2] == node {
            return Some(fields[0].to_string());
        }
    }
    None
}

// Rebuilds the snapshot view, on this task rather than on the worker loop. Skipped entirely when
// there is nothing bound and nothing has ever been published, so a deployment with no profiles pays
// for one `binding::all()` — a cached read — every five seconds and nothing else.
fn publish_view() {
    let bindings = binding::all();
    if bindings.is_empty() && lock(published_view()).is_none() {
        return;
    }
    let view = if bindings.is_empty() {
        serde_json::json!([])
    } else {
        build_view()
    };
    *lock(published_view()) = Some(view);
}

// The task itself. Started only where a coordinator's roster is — a node runs no scheduler and has
// nothing to boot.
pub fn spawn() {
    tokio::spawn(async {
        attempt::reconcile_on_start();
        // The loop runs whether or not a profile exists today. Bailing out here would have meant a
        // profile written after startup never took effect until the process restarted, and the
        // idle pass costs one mutex and a clone — `reconcile_once` reads nothing off disk until
        // something is actually waiting for a node.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(RECONCILE_SECS)).await;
            reconcile_once().await;
            publish_view();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnworker::link::spec::NodePurpose;

    fn binding(node: &str, purpose: NodePurpose, encoders: &[&str]) -> BootBinding {
        BootBinding {
            node: node.to_string(),
            profile: "p".to_string(),
            profile_revision: 1,
            purpose,
            expected_encoders: encoders.iter().map(|e| e.to_string()).collect(),
            image_revision: "v1".to_string(),
            proven_encoders: Vec::new(),
            proven_image_revision: String::new(),
            capability_mismatch: None,
            created_at: 0,
        }
    }

    fn tokens(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_node_with_no_link_token_is_never_booted() {
        let b = binding("gpu-1", NodePurpose::Gpu, &["h264_nvenc"]);
        let err = eligibility(&b, &tokens(&[]), &Default::default()).unwrap_err();
        assert_eq!(err, Ineligible::NoToken);
    }

    #[test]
    fn a_registered_node_is_not_a_boot_candidate() {
        let b = binding("gpu-1", NodePurpose::Gpu, &["h264_nvenc"]);
        let mut online = std::collections::HashMap::new();
        online.insert("gpu-1".to_string(), true);
        let err = eligibility(&b, &tokens(&["gpu-1"]), &online).unwrap_err();
        assert_eq!(err, Ineligible::AlreadyOnline);
    }

    #[test]
    fn a_drained_node_is_never_auto_booted() {
        let b = binding("gpu-1", NodePurpose::Gpu, &["h264_nvenc"]);
        let mut online = std::collections::HashMap::new();
        online.insert("gpu-1".to_string(), false);
        let err = eligibility(&b, &tokens(&["gpu-1"]), &online).unwrap_err();
        assert_eq!(err, Ineligible::Drained);
    }

    #[test]
    fn a_capability_mismatch_suppresses_further_rentals() {
        let mut b = binding("gpu-1", NodePurpose::Gpu, &["h264_nvenc"]);
        b.capability_mismatch = Some("registered without h264_nvenc".to_string());
        // The profile cannot be read under `cargo test` — there is no `DB/` — so the missing-profile
        // reason wins. What this pins is that the mismatch is *not* consulted before the profile is
        // loaded: it has to be compared against the current revision, and there is no revision
        // without a profile.
        let err = eligibility(&b, &tokens(&["gpu-1"]), &Default::default()).unwrap_err();
        assert!(
            matches!(err, Ineligible::ProfileMissing(_)),
            "expected the profile to be loaded first, got {err:?}"
        );
    }

    #[test]
    fn every_ineligibility_says_something_an_operator_can_act_on() {
        for reason in [
            Ineligible::NoToken,
            Ineligible::ProfileMissing("no such file".into()),
            Ineligible::ProfileDisabled,
            Ineligible::CapabilityMismatch("no nvenc".into()),
            Ineligible::AlreadyOnline,
            Ineligible::AlreadyBooting,
            Ineligible::OutcomeUnknown("timeout".into()),
            Ineligible::CoolingDown(60),
            Ineligible::AttemptsExhausted(3),
            Ineligible::Drained,
        ] {
            assert!(!reason.describe().is_empty());
        }
    }

    fn demand(job_id: u64, preset: &str) -> Demand {
        Demand {
            job_id,
            preset: preset.to_string(),
            server: None,
        }
    }

    // The preset table is what decides hardware and codec, so these use a real GPU preset name
    // rather than a made-up one — the point of the rule is that it agrees with `pick_node`.
    fn gpu_preset() -> String {
        crate::lib::mpeg::preset::BUILTIN_PRESET_NAMES
            .iter()
            .find(|p| {
                preset::hardware_for(p) == preset::PresetHardware::Gpu
                    && preset::video_codec_for(p).is_some()
            })
            .expect("the built-in table has at least one GPU preset")
            .to_string()
    }

    #[test]
    fn a_machine_already_booting_covers_the_demand_it_could_serve() {
        let preset = gpu_preset();
        let codec = crate::lib::mpeg::preset::video_codec_for(&preset).unwrap();
        let b = binding("gpu-1", NodePurpose::Gpu, &[&codec]);
        let jobs = vec![demand(1, &preset)];
        // Nothing starting: the job is unmet.
        assert_eq!(unmet_demand(&jobs, &[b.clone()], &[]).len(), 1);
        // The same machine mid-boot covers it, so no second rental.
        assert!(unmet_demand(&jobs, &[b], &["gpu-1".to_string()]).is_empty());
    }

    #[test]
    fn one_booting_machine_covers_a_whole_queue_of_the_work_it_serves() {
        let preset = gpu_preset();
        let codec = crate::lib::mpeg::preset::video_codec_for(&preset).unwrap();
        let b = binding("gpu-1", NodePurpose::Gpu, &[&codec]);
        let jobs: Vec<Demand> = (1..=10).map(|i| demand(i, &preset)).collect();
        // Ten waiting episodes must not become ten rented machines.
        assert!(unmet_demand(&jobs, &[b], &["gpu-1".to_string()]).is_empty());
    }

    #[test]
    fn a_booting_machine_does_not_cover_demand_it_could_not_serve() {
        let preset = gpu_preset();
        // A CPU box on its way is not an answer to GPU demand.
        let b = binding("cpu-1", NodePurpose::Cpu, &[]);
        let jobs = vec![demand(1, &preset)];
        assert_eq!(unmet_demand(&jobs, &[b], &["cpu-1".to_string()]).len(), 1);
    }

    #[test]
    fn a_booting_node_with_no_binding_covers_nothing() {
        let preset = gpu_preset();
        let jobs = vec![demand(1, &preset)];
        // A node in flight that no binding describes cannot be reasoned about, so it must not be
        // treated as capacity for anything.
        assert_eq!(unmet_demand(&jobs, &[], &["ghost".to_string()]).len(), 1);
    }

    #[test]
    fn demand_publishes_and_reads_back() {
        publish_demand(vec![Demand {
            job_id: 7,
            preset: "x264-1080p".into(),
            server: None,
        }]);
        assert_eq!(current_demand().len(), 1);
        publish_demand(Vec::new());
        assert!(current_demand().is_empty());
    }

    #[tokio::test]
    async fn no_queue_means_no_boot() {
        publish_demand(Vec::new());
        assert!(reconcile_once().await.is_empty());
    }

    #[test]
    fn a_long_error_is_shortened_to_one_line() {
        let out = shorten(&format!("a\nb{}", "x".repeat(400)));
        assert!(!out.contains('\n'));
        assert!(out.chars().count() <= 181, "{}", out.chars().count());
    }
}
