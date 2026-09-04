// Which profile belongs to which node, and what that node is expected to be able to do once it
// starts. A binding is created by `/gentoken link:<node> boot:<profile>` — before the machine has
// ever registered, which is the whole point: the roster only ever learns about a node that has
// already said hello, and a node that has to be booted before it can say anything would never
// appear there in time to be chosen.
//
// The binding is stored beside the roster rather than in the token line so the token format stays
// what every existing deployment already has on disk.

use serde::{Deserialize, Serialize};

use crate::lib::env::standard::{API_TOKENS_PATH, LINK_BOOT_BINDINGS_PATH};
use crate::lib::sync::lock;
use crate::pnworker::link::spec::NodePurpose;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BootBinding {
    pub node: String,
    pub profile: String,
    // The profile revision this binding was created against. An edit to the profile does not
    // invalidate the binding, but it does invalidate a capability proof taken under the old one.
    #[serde(default)]
    pub profile_revision: u64,
    #[serde(default)]
    pub purpose: NodePurpose,
    // What the profile said this machine would be able to encode with, copied here at mint time.
    // The scheduler reads this to decide whether an offline node could serve a waiting job, which
    // is a question `link_nodes.json` cannot answer: that file only holds machines that registered.
    #[serde(default)]
    pub expected_encoders: Vec<String>,
    #[serde(default)]
    pub image_revision: String,
    // What the machine actually proved at registration, and the image revision it proved it under.
    // Kept apart from `expected_encoders` on purpose: a claim and a measurement are different
    // things, and a proof taken against hardware the profile no longer rents is not a proof.
    #[serde(default)]
    pub proven_encoders: Vec<String>,
    #[serde(default)]
    pub proven_image_revision: String,
    // Set when a booted machine registered without the encoders it was rented for. It suppresses
    // further automatic boots for this profile revision — renting the same wrong box on a loop is
    // the one failure here that costs real money — and is cleared by editing the profile or by the
    // node proving the encoders on a later registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_mismatch: Option<String>,
    #[serde(default)]
    pub created_at: u64,
}

impl BootBinding {
    // Whether the machine this binding describes could take a job needing this hardware and codec,
    // if it were running. Deliberately the same two questions `select_node` asks of a registered
    // node — purpose, then encoder — so an offline candidate is judged by the rule that will judge
    // it again once it is online, and a boot cannot be triggered for work the node would decline.
    pub fn could_serve(
        &self,
        hardware: crate::lib::mpeg::preset::PresetHardware,
        required_encoder: Option<&str>,
    ) -> bool {
        if !self.purpose.accepts(hardware) {
            return false;
        }
        match required_encoder {
            // A codec the preset names is matched against what this machine is expected to have.
            // An empty expectation cannot satisfy it: renting on the hope that the box turns out to
            // have an NVENC is how a GPU queue quietly becomes a bill.
            Some(required) => self.expected_encoders.iter().any(|e| e == required),
            None => true,
        }
    }
}

// The cached set, with the file modification time it was read at. Editing the file by hand is the
// documented way to remove or retarget a binding, so a cache that could not notice one would make
// that instruction false — the same reason the API's token cache keys on the token file's mtime.
type Cache = Option<(Option<std::time::SystemTime>, Vec<BootBinding>)>;

fn store() -> &'static Mutex<Cache> {
    static STORE: OnceLock<Mutex<Cache>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

fn file_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(LINK_BOOT_BINDINGS_PATH)
        .and_then(|m| m.modified())
        .ok()
}

// Fills or refreshes the cache in place, and hands back a mutable reference to the bindings.
fn ensure_loaded(guard: &mut Cache) -> &mut Vec<BootBinding> {
    let mtime = file_mtime();
    match guard {
        Some((cached_at, _)) if *cached_at == mtime => {}
        _ => *guard = Some((mtime, read_file())),
    }
    &mut guard.as_mut().expect("filled above").1
}

fn read_file() -> Vec<BootBinding> {
    let Ok(contents) = std::fs::read_to_string(LINK_BOOT_BINDINGS_PATH) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<BootBinding>>(&contents) {
        Ok(bindings) => bindings,
        Err(error) => {
            // The same treatment `link_nodes.json` gets: an unreadable file is set aside rather
            // than overwritten by the first save after it, because the first save would take every
            // binding with it and the only symptom would be nodes that stopped being booted.
            let kept = format!("{LINK_BOOT_BINDINGS_PATH}.unreadable");
            match std::fs::rename(LINK_BOOT_BINDINGS_PATH, &kept) {
                Ok(()) => eprintln!(
                    "[boot] bindings at {LINK_BOOT_BINDINGS_PATH} are unreadable ({error}); kept as {kept} and starting empty"
                ),
                Err(e) => eprintln!(
                    "[boot] bindings at {LINK_BOOT_BINDINGS_PATH} are unreadable ({error}) and could not be set aside ({e}); starting empty"
                ),
            }
            Vec::new()
        }
    }
}

pub fn all() -> Vec<BootBinding> {
    let mut guard = lock(store());
    ensure_loaded(&mut guard).clone()
}

pub fn for_node(node: &str) -> Option<BootBinding> {
    all().into_iter().find(|b| b.node == node)
}

// Writes the whole set through a temporary file, like the roster does. The caller holds no lock
// across the write: a binding change is an operator action, not something on a hot path. Every
// caller drops the cache afterwards, because the write moves the mtime the cache is keyed on.
fn save(bindings: &[BootBinding]) -> Result<(), String> {
    let body = serde_json::to_string_pretty(bindings).map_err(|e| e.to_string())?;
    let path = std::path::Path::new(LINK_BOOT_BINDINGS_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, body).map_err(|e| e.to_string())?;
    restrict(&temporary);
    std::fs::rename(&temporary, path).map_err(|e| {
        std::fs::remove_file(&temporary).ok();
        e.to_string()
    })
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

// Creates or replaces one node's binding. Refuses a second, different profile for a node that
// already has one: two valid tokens for the same machine naming different profiles is a question
// with no right answer, and picking one silently is how a node gets rented from the wrong provider.
pub fn bind(binding: BootBinding) -> Result<(), String> {
    let mut guard = lock(store());
    let bindings = ensure_loaded(&mut guard);
    if let Some(existing) = bindings.iter().find(|b| b.node == binding.node) {
        if existing.profile != binding.profile {
            return Err(format!(
                "`{}` is already bound to boot profile `{}`. Remove that binding from {} before binding it to `{}`.",
                binding.node, existing.profile, LINK_BOOT_BINDINGS_PATH, binding.profile
            ));
        }
    }
    bindings.retain(|b| b.node != binding.node);
    bindings.push(binding);
    bindings.sort_by(|a, b| a.node.cmp(&b.node));
    let snapshot = bindings.clone();
    drop(guard);
    let result = save(&snapshot);
    // The write moved the file's mtime, so the cached copy is now stale by its own rule. Dropping
    // it is cheaper than trying to predict what the filesystem recorded.
    invalidate_cache();
    result
}

// Records what a node proved when it registered. Called from the register path, so a machine that
// came up without the encoders it was rented for stops being rented again for the same demand.
pub fn record_registration(node: &str, encoders: &[String]) {
    let mut guard = lock(store());
    let bindings = ensure_loaded(&mut guard);
    let Some(binding) = bindings.iter_mut().find(|b| b.node == node) else {
        return;
    };
    let missing: Vec<String> = binding
        .expected_encoders
        .iter()
        .filter(|expected| !encoders.iter().any(|have| &have == expected))
        .cloned()
        .collect();
    let mismatch = (!missing.is_empty()).then(|| {
        format!(
            "registered without {}; automatic boot is suppressed until the profile or its hardware changes",
            missing.join(", ")
        )
    });
    // A node registers every thirty seconds for as long as it runs, and almost all of those say
    // exactly what the last one did. Comparing the whole recorded proof before writing is what
    // keeps this from rewriting the bindings file twice a minute per node.
    if binding.proven_encoders == encoders
        && binding.proven_image_revision == binding.image_revision
        && binding.capability_mismatch == mismatch
    {
        return;
    }
    let changed_mismatch = binding.capability_mismatch != mismatch;
    binding.proven_encoders = encoders.to_vec();
    binding.proven_image_revision = binding.image_revision.clone();
    binding.capability_mismatch = mismatch;
    if changed_mismatch {
        match &binding.capability_mismatch {
            Some(reason) => eprintln!("[boot] {node} | {reason}"),
            None => println!("[boot] {node} | proved every expected encoder"),
        }
    }
    let snapshot = bindings.clone();
    drop(guard);
    if let Err(e) = save(&snapshot) {
        eprintln!("[boot] could not record {node}'s capability proof: {e}");
    }
    invalidate_cache();
}

// Forgets the cached copy so the next read comes off disk. Used after an operator edits the file by
// hand, which is the documented way to remove or retarget a binding.
pub fn invalidate_cache() {
    *lock(store()) = None;
}

// Brings one binding's expectations back in line with its profile. A profile is a file an operator
// edits, and the binding copied its capabilities at mint time — without this, adding an encoder to a
// profile would never reach the scheduler, and the promise that editing a profile affects future
// attempts would be false.
//
// The revision moving is also what clears a capability mismatch: a machine that came up without the
// encoders it was rented for stops being rented again, and changing the profile is how an operator
// says the plan is different now.
pub fn refresh_expectations(node: &str, revision: u64, capabilities: &super::profile::Capabilities) {
    let mut guard = lock(store());
    let bindings = ensure_loaded(&mut guard);
    let Some(binding) = bindings.iter_mut().find(|b| b.node == node) else {
        return;
    };
    if binding.profile_revision == revision
        && binding.expected_encoders == capabilities.encoders
        && binding.image_revision == capabilities.image_revision
    {
        return;
    }
    let cleared = binding.capability_mismatch.take();
    binding.profile_revision = revision;
    binding.expected_encoders = capabilities.encoders.clone();
    binding.image_revision = capabilities.image_revision.clone();
    if cleared.is_some() {
        println!("[boot] {node} | its profile changed; the recorded capability mismatch is cleared");
    }
    let snapshot = bindings.clone();
    drop(guard);
    if let Err(e) = save(&snapshot) {
        eprintln!("[boot] could not record {node}'s refreshed expectations: {e}");
    }
    invalidate_cache();
}

// The node names that currently have a link token. A binding without one is inert: the plan's rule
// is that removing the last authorizing token disables automatic boot for that node, and this is
// what that rule reads.
//
// This parses `api.pandora` for one field rather than reusing the API's own reader, which builds a
// whole authorisation record per token behind a private type. The question here is narrower and the
// answer is a set of names.
pub fn nodes_with_tokens() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(contents) = std::fs::read_to_string(API_TOKENS_PATH) else {
        return out;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
        if fields.len() >= 3 && fields[1] == "link" && !fields[2].is_empty() {
            out.insert(fields[2].to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::mpeg::preset::PresetHardware;

    fn binding(purpose: NodePurpose, encoders: &[&str]) -> BootBinding {
        BootBinding {
            node: "gpu-1".to_string(),
            profile: "gpu".to_string(),
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

    #[test]
    fn a_cpu_binding_does_not_serve_gpu_demand() {
        let b = binding(NodePurpose::Cpu, &[]);
        assert!(!b.could_serve(PresetHardware::Gpu, Some("h264_nvenc")));
        assert!(b.could_serve(PresetHardware::Cpu, None));
    }

    #[test]
    fn a_gpu_binding_must_name_the_codec_the_preset_needs() {
        let b = binding(NodePurpose::Gpu, &["h264_nvenc"]);
        assert!(b.could_serve(PresetHardware::Gpu, Some("h264_nvenc")));
        // The review's case: advertising one NVENC codec does not make a box able to serve another.
        assert!(!b.could_serve(PresetHardware::Gpu, Some("av1_nvenc")));
    }

    #[test]
    fn a_gpu_binding_with_no_expected_encoders_cannot_satisfy_a_codec_requirement() {
        let b = binding(NodePurpose::Gpu, &[]);
        assert!(!b.could_serve(PresetHardware::Gpu, Some("h264_nvenc")));
    }

    #[test]
    fn a_both_binding_serves_either_side() {
        let b = binding(NodePurpose::Both, &["h264_nvenc"]);
        assert!(b.could_serve(PresetHardware::Cpu, None));
        assert!(b.could_serve(PresetHardware::Gpu, Some("h264_nvenc")));
    }

    // The rule `record_registration` applies, over a binding rather than through the file: a
    // machine proves what it proves, and only what is missing from the profile's expectation
    // suppresses the next rental.
    fn missing_from(binding: &BootBinding, encoders: &[&str]) -> Vec<String> {
        binding
            .expected_encoders
            .iter()
            .filter(|expected| !encoders.iter().any(|have| have == &expected.as_str()))
            .cloned()
            .collect()
    }

    #[test]
    fn a_machine_that_proves_what_was_expected_leaves_no_mismatch() {
        let b = binding(NodePurpose::Gpu, &["h264_nvenc"]);
        assert!(missing_from(&b, &["h264_nvenc", "hevc_nvenc"]).is_empty());
    }

    #[test]
    fn a_machine_missing_an_expected_encoder_is_recorded_as_a_mismatch() {
        let b = binding(NodePurpose::Gpu, &["av1_nvenc"]);
        // The expensive case: the box came up, cost money, and cannot do the work it was for.
        assert_eq!(missing_from(&b, &["h264_nvenc"]), vec!["av1_nvenc"]);
    }

    #[test]
    fn a_binding_expecting_nothing_can_never_mismatch() {
        let b = binding(NodePurpose::Cpu, &[]);
        assert!(missing_from(&b, &[]).is_empty());
    }

    #[test]
    fn token_lines_are_read_for_their_node_field_only() {
        // Mirrors the shapes in `api.pandora`: a label comment, a plain token, a local token and
        // two link tokens, one of them privileged-marked (which a node line never is, but the
        // parse must not care).
        let contents = "; note\nabc\ndef|local|123\nghi|link|gpu-1|gpu\njkl|link|cpu-2\n";
        let mut found = std::collections::HashSet::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
            if fields.len() >= 3 && fields[1] == "link" && !fields[2].is_empty() {
                found.insert(fields[2].to_string());
            }
        }
        assert_eq!(found.len(), 2);
        assert!(found.contains("gpu-1"));
        assert!(found.contains("cpu-2"));
    }
}
