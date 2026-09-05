use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lib::sync::lock;
use crate::pnworker::util::{CONCAT_KINDS, ConcatConfig, ConcatKind};

// Fonts and concat videos are the things a node needs that do not travel in a job spec, and a
// missing font does not fail — libass substitutes one and ships a release in the wrong typeface.
// That is the failure this module exists to make impossible: a node syncs the coordinator's whole
// asset set, and a job whose revision it has not synced is refused rather than encoded.
//
// The corpus is compared by content, never by name or timestamp. Two machines agree when their
// files hash the same, which is the only definition that survives a copy, a re-download, or a
// filesystem that rounds mtimes.

pub const FONT_ROOT: &str = "DB/fontconfig";
// Synced concat variants live apart from anything an operator hand-placed, so a reconcile can never
// overwrite a coordinator's own intro or outro folder on a machine that is both. The two kinds get
// their own roots so that groups sharing a name do not collide.
pub const LINK_INTRO_ROOT: &str = "DB/cache/link-intros";
pub const LINK_OUTRO_ROOT: &str = "DB/cache/link-outros";
const REVISION_FILE: &str = "DB/cache/link-assets-revision";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Font,
    Intro,
    Outro,
}

impl AssetKind {
    // The concat kind this asset belongs to, or None for a font. Every path decision below is made
    // by this rather than by a second match on the variant, so adding a kind adds one arm.
    pub fn concat_kind(self) -> Option<ConcatKind> {
        match self {
            AssetKind::Font => None,
            AssetKind::Intro => Some(ConcatKind::Intro),
            AssetKind::Outro => Some(ConcatKind::Outro),
        }
    }
}

fn asset_kind_of(kind: ConcatKind) -> AssetKind {
    match kind {
        ConcatKind::Intro => AssetKind::Intro,
        ConcatKind::Outro => AssetKind::Outro,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetEntry {
    pub hash: String,
    pub kind: AssetKind,
    // The font bucket under `DB/fontconfig`, or the intro/outro group name.
    pub group: String,
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetManifest {
    // Derived from the content of every entry, so it changes exactly when the corpus does and
    // needs no counter to bump and no hook in `/cfont` to remember to call.
    pub revision: String,
    pub entries: Vec<AssetEntry>,
}

impl AssetManifest {
    pub fn find(&self, hash: &str) -> Option<&AssetEntry> {
        self.entries.iter().find(|entry| entry.hash == hash)
    }

    pub fn concat_entries(&self, kind: ConcatKind, group: &str) -> impl Iterator<Item = &AssetEntry> {
        let wanted = asset_kind_of(kind);
        self.entries
            .iter()
            .filter(move |entry| entry.kind == wanted && entry.group == group)
    }
}

// Hashing a whole font corpus on every request would be absurd, and re-reading it on every renew
// only slightly less so. Files are hashed once per (path, mtime, len) and the assembled manifest is
// held for a minute — long enough that a node polling every ten seconds costs one scan, short
// enough that an operator adding a font sees it take effect while they are still watching.
fn hash_cache() -> &'static Mutex<HashMap<PathBuf, (SystemTime, u64, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (SystemTime, u64, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn hash_file(path: &Path) -> Option<(String, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let len = metadata.len();
    let mtime = metadata.modified().ok()?;
    if let Some((cached_mtime, cached_len, hash)) = lock(hash_cache()).get(path) {
        if *cached_mtime == mtime && *cached_len == len {
            return Some((hash.clone(), len));
        }
    }
    let bytes = std::fs::read(path).ok()?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    lock(hash_cache()).insert(path.to_path_buf(), (mtime, len, hash.clone()));
    Some((hash, len))
}

fn revision_of(entries: &[AssetEntry]) -> String {
    let mut lines = entries
        .iter()
        .map(|entry| {
            format!(
                "{:?}/{}/{}/{}",
                entry.kind, entry.group, entry.name, entry.hash
            )
        })
        .collect::<Vec<_>>();
    lines.sort();
    format!("{:x}", Sha256::digest(lines.join("\n").as_bytes()))
}

// Variants pnmpeg produced to make a retained intro concat-compatible with one episode's exact
// stream properties, and the temporaries it writes them through. They are a per-machine cache
// derived from the corpus rather than part of it: they appear in the coordinator's own concat
// folder as it encodes, so counting them would move the revision — and re-sync every node — every
// time a new output format was encountered. A node regenerates its own on demand.
const GENERATED_INTRO_PREFIX: &str = "pnmpeg_compat_";

fn is_generated_intro(name: &str) -> bool {
    name.starts_with(GENERATED_INTRO_PREFIX) || name.contains(".tmp.")
}

fn plain_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_file()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    files.sort();
    files
}

// Builds the coordinator's manifest: every font bucket under `DB/fontconfig`, and every file in
// every folder an intro or outro group resolves to.
pub fn build_manifest() -> AssetManifest {
    let mut entries = Vec::new();
    if let Ok(buckets) = std::fs::read_dir(FONT_ROOT) {
        let mut buckets = buckets
            .flatten()
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        buckets.sort();
        for bucket in buckets {
            let group = bucket
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if group.trim().is_empty() {
                continue;
            }
            for file in plain_files(&bucket) {
                push_entry(&mut entries, AssetKind::Font, &group, &file);
            }
        }
    }
    for kind in CONCAT_KINDS {
        let config = ConcatConfig::load_kind(kind);
        let mut groups = config.groups.iter().collect::<Vec<_>>();
        groups.sort();
        for (group, folder) in groups {
            if folder.trim().is_empty() {
                continue;
            }
            for file in plain_files(Path::new(folder)) {
                let generated = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(is_generated_intro)
                    .unwrap_or(false);
                if generated {
                    continue;
                }
                push_entry(&mut entries, asset_kind_of(kind), group, &file);
            }
        }
    }
    let revision = revision_of(&entries);
    AssetManifest { revision, entries }
}

fn push_entry(entries: &mut Vec<AssetEntry>, kind: AssetKind, group: &str, file: &Path) {
    let Some(name) = file.file_name().map(|name| name.to_string_lossy().to_string()) else {
        return;
    };
    let Some((hash, bytes)) = hash_file(file) else {
        return;
    };
    entries.push(AssetEntry {
        hash,
        kind,
        group: group.to_string(),
        name,
        bytes,
    });
}

pub fn manifest() -> AssetManifest {
    static CACHE: OnceLock<Mutex<Option<(Instant, AssetManifest)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((built_at, cached)) = lock(cache).as_ref() {
        if built_at.elapsed() < Duration::from_secs(60) {
            return cached.clone();
        }
    }
    let fresh = build_manifest();
    *lock(cache) = Some((Instant::now(), fresh.clone()));
    fresh
}

// Keeps the manifest cache warm off the runtime.
//
// `manifest()` is read from two places that must never block: the axum link routes, and — through
// `build_spec` — `pn_worker`'s loop, which is the single thread every job on this machine passes
// through. A cold cache there means walking every font bucket and intro folder and hashing
// anything whose mtime moved, with the whole queue stopped behind it. It is not a failure anything
// reports; it is a coordinator that pauses for a second every minute, and on a large corpus for
// rather longer than that.
//
// So the walk is done here instead, on a blocking thread, just often enough that the cache the
// loop reads is always one somebody else built. Coordinator-side only: a node's corpus is the one
// it syncs from the coordinator, and building a manifest of it would answer a question nobody asks.
pub fn spawn_manifest_refresher() {
    // Under the 60-second cache lifetime, so the entry a caller finds has always been replaced
    // rather than expired.
    const REFRESH_SECS: u64 = 30;
    tokio::spawn(async {
        loop {
            tokio::task::spawn_blocking(|| {
                let _ = manifest();
            })
            .await
            .ok();
            tokio::time::sleep(std::time::Duration::from_secs(REFRESH_SECS)).await;
        }
    });
}

// Serving strictly by hash, and only for a hash the current manifest lists, is what keeps this
// from being an arbitrary-file read: a node cannot ask for a path, only for content the
// coordinator has already published.
pub fn read_asset(hash: &str) -> Option<(AssetEntry, Vec<u8>)> {
    let manifest = manifest();
    let entry = manifest.find(hash)?.clone();
    let path = source_path(&entry)?;
    let bytes = std::fs::read(&path).ok()?;
    if format!("{:x}", Sha256::digest(&bytes)) != entry.hash {
        // The file changed under the cached manifest. Answering nothing is right: the node will
        // reconcile against the next revision rather than take a copy of something else.
        return None;
    }
    Some((entry, bytes))
}

// A name and a group both arrive off the wire and both become path components, so neither may
// address anything outside the root it belongs to.
fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.starts_with('.')
}

// Where the coordinator reads an entry from. Fonts sit under `DB/fontconfig/<bucket>`, but a concat
// group lives wherever it resolves to in `intros.toml` or `outros.toml` — which is emphatically not
// where a node puts it. Conflating the two served every font correctly and answered 404 for every
// intro.
pub fn source_path(entry: &AssetEntry) -> Option<PathBuf> {
    if !safe_component(&entry.name) || !safe_component(&entry.group) {
        return None;
    }
    Some(match entry.kind.concat_kind() {
        None => PathBuf::from(FONT_ROOT).join(&entry.group).join(&entry.name),
        Some(kind) => {
            PathBuf::from(ConcatConfig::load_kind(kind).resolve(&entry.group)?).join(&entry.name)
        }
    })
}

// Where a node writes an entry. Fonts go to the bucket the startup installer already copies into
// the OS font path; intros and outros go to the node's own synced folders, deliberately apart from
// anything an operator hand-placed, since pnmpeg writes compatibility variants back into whatever
// folder it is given.
pub fn install_path(entry: &AssetEntry) -> Option<PathBuf> {
    if !safe_component(&entry.name) || !safe_component(&entry.group) {
        return None;
    }
    Some(match entry.kind.concat_kind() {
        None => PathBuf::from(FONT_ROOT).join(&entry.group).join(&entry.name),
        Some(kind) => concat_dir(kind, &entry.group).join(&entry.name),
    })
}

// On a coordinator a concat group resolves through `intros.toml` or `outros.toml`; on a node it is
// always this folder, because the node materialised it and nothing else knows the group exists.
pub fn concat_dir(kind: ConcatKind, group: &str) -> PathBuf {
    PathBuf::from(concat_root(kind)).join(group)
}

fn concat_root(kind: ConcatKind) -> &'static str {
    match kind {
        ConcatKind::Intro => LINK_INTRO_ROOT,
        ConcatKind::Outro => LINK_OUTRO_ROOT,
    }
}

// Entries this machine does not already hold, byte for byte.
pub fn missing(manifest: &AssetManifest) -> Vec<AssetEntry> {
    manifest
        .entries
        .iter()
        .filter(|entry| {
            let Some(path) = install_path(entry) else {
                return false;
            };
            match hash_file(&path) {
                Some((hash, _)) => hash != entry.hash,
                None => true,
            }
        })
        .cloned()
        .collect()
}

// Files under the node's synced concat roots that the manifest no longer names, removed.
//
// `missing` only ever adds, which leaves the one change a fetch-what-is-missing sync cannot see:
// a deletion. The coordinator's revision moves, nothing is missing, and the node records the new
// revision while still holding a file the corpus no longer has — so two machines agreeing on a
// revision did not mean they held the same corpus. For an intro or outro that is not cosmetic: the
// whole folder is handed to pnmpeg, which picks a variant out of it, so a retired one goes on
// shipping from every node that ever had it.
//
// Only concat groups are pruned. `DB/fontconfig` is shared with fonts an operator placed by hand,
// the startup installer has already copied them into the OS font path where deleting the bucket
// copy would not remove them, and a font the corpus no longer lists causes no substitution — which
// is the failure the corpus exists to prevent. The concat roots are the node's own and hold nothing
// else, which is exactly why they are kept apart from the coordinator's.
pub fn prune_concat(manifest: &AssetManifest) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for kind in CONCAT_KINDS {
        let Ok(groups) = std::fs::read_dir(concat_root(kind)) else {
            continue;
        };
        for group_entry in groups.flatten() {
            if !group_entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let directory = group_entry.path();
            let group = group_entry.file_name().to_string_lossy().to_string();
            let wanted = manifest
                .concat_entries(kind, &group)
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>();
            for file in plain_files(&directory) {
                let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if wanted.iter().any(|kept| kept == name) {
                    continue;
                }
                // Generated variants go with everything else. They are derived from the files this
                // group held, and this only runs when that set has changed.
                if std::fs::remove_file(&file).is_ok() {
                    removed.push(file);
                }
            }
            // A group the corpus dropped entirely leaves its directory behind, which
            // `concat_group_is_populated` would go on treating as a group with no files.
            if wanted.is_empty() {
                std::fs::remove_dir(&directory).ok();
            }
        }
    }
    removed
}

pub fn write_asset(entry: &AssetEntry, bytes: &[u8]) -> Result<(), String> {
    if format!("{:x}", Sha256::digest(bytes)) != entry.hash {
        return Err(format!("{} did not hash to what the manifest said", entry.name));
    }
    let Some(path) = install_path(entry) else {
        return Err(format!("{}/{} is not a name this can write", entry.group, entry.name));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Written beside and renamed, so a half-downloaded font is never a font libass can find.
    let temporary = path.with_extension("link-part");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| {
        std::fs::remove_file(&temporary).ok();
        e.to_string()
    })
}

pub fn local_revision() -> Option<String> {
    std::fs::read_to_string(REVISION_FILE)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn record_revision(revision: &str) {
    let path = Path::new(REVISION_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, revision).ok();
}

// A synced concat group that materialised nothing would hand pnmpeg an empty folder and quietly
// produce a release with no intro or no outro, which is the same class of failure as a substituted
// font.
pub fn concat_group_is_populated(kind: ConcatKind, group: &str) -> bool {
    !plain_files(&concat_dir(kind, group)).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: AssetKind, group: &str, name: &str, hash: &str) -> AssetEntry {
        AssetEntry {
            hash: hash.to_string(),
            kind,
            group: group.to_string(),
            name: name.to_string(),
            bytes: 0,
        }
    }

    // The revision is the whole safety mechanism: it has to change when any file's content changes
    // and be identical on two machines holding the same corpus, regardless of scan order.
    #[test]
    fn the_revision_follows_content_and_not_order() {
        let a = entry(AssetKind::Font, "main", "a.ttf", "1111");
        let b = entry(AssetKind::Font, "main", "b.ttf", "2222");
        let forwards = revision_of(&[a.clone(), b.clone()]);
        let backwards = revision_of(&[b.clone(), a.clone()]);
        assert_eq!(forwards, backwards);

        let changed = entry(AssetKind::Font, "main", "b.ttf", "3333");
        assert_ne!(forwards, revision_of(&[a.clone(), changed]));

        // A font moving bucket is a different corpus, because that is where it gets installed from.
        let moved = entry(AssetKind::Font, "other", "b.ttf", "2222");
        assert_ne!(forwards, revision_of(&[a, moved]));
    }

    // Entries name a bucket and a filename, and both come off the wire. Neither may address
    // anything outside the roots these are allowed to touch — on either side of the link.
    #[test]
    fn entry_names_cannot_escape_their_root() {
        for build in [install_path, source_path] {
            assert!(build(&entry(AssetKind::Font, "main", "../../etc/passwd", "x")).is_none());
            assert!(build(&entry(AssetKind::Font, "../..", "a.ttf", "x")).is_none());
            assert!(build(&entry(AssetKind::Intro, "grp", "..", "x")).is_none());
            assert!(build(&entry(AssetKind::Font, "main", ".hidden", "x")).is_none());
            assert!(build(&entry(AssetKind::Font, "main", "", "x")).is_none());
        }
        assert!(install_path(&entry(AssetKind::Font, "main", "Roboto.ttf", "x")).is_some());
        assert!(source_path(&entry(AssetKind::Font, "main", "Roboto.ttf", "x")).is_some());
    }

    // A font is read from and written to the same place, so conflating the two sides looked
    // correct for 105 of 142 entries and answered 404 for the rest: a concat group lives wherever
    // its config points on the coordinator, and under the node's own synced root on a node.
    #[test]
    fn a_concat_group_is_not_read_from_where_a_node_writes_it() {
        for (kind, root) in [
            (AssetKind::Intro, LINK_INTRO_ROOT),
            (AssetKind::Outro, LINK_OUTRO_ROOT),
        ] {
            let asset = entry(kind, "opening", "op1.mkv", "x");
            let install = install_path(&asset).expect("a node must have somewhere to put it");
            assert!(install.starts_with(root), "{}", install.display());
            // The source resolves through the concat config, so it is only `Some` where that group
            // is actually configured — and it is never the node's synced root.
            if let Some(source) = source_path(&asset) {
                assert_ne!(source, install);
                assert!(!source.starts_with(root), "{}", source.display());
            }
        }

        // Intro and outro groups may share a name, and must not then share a folder.
        let name = "opening";
        assert_ne!(
            install_path(&entry(AssetKind::Intro, name, "a.mkv", "x")),
            install_path(&entry(AssetKind::Outro, name, "a.mkv", "x")),
        );

        // A font is genuinely the same path on both sides; that coincidence is what hid the bug.
        let font = entry(AssetKind::Font, "main", "Roboto.ttf", "x");
        assert_eq!(source_path(&font), install_path(&font));
    }

    // pnmpeg writes its compatibility variants into whichever intro folder it is handed, which on
    // a coordinator is the folder the manifest is built from. Counting them would move the
    // revision — and re-sync every node — every time an episode with new stream properties was
    // encoded, and would sync a cache one machine derived to every other machine.
    #[test]
    fn generated_intro_variants_are_not_part_of_the_corpus() {
        assert!(is_generated_intro("pnmpeg_compat_9f8e.mp4"));
        assert!(is_generated_intro("pnmpeg_compat_9f8e.tmp.mp4"));
        assert!(!is_generated_intro("opening.mkv"));
        assert!(!is_generated_intro("op1_v2.mp4"));
    }

    // The failure this exists for: `missing` only adds, so a corpus that lost a file left every
    // node holding it while reporting itself level. pnmpeg picks a variant out of the whole
    // folder, so that is a retired intro going out on real releases.
    #[test]
    fn a_dropped_concat_file_is_removed_from_the_nodes_own_root() {
        let group = format!("prune-test-{}", std::process::id());
        let directories = CONCAT_KINDS.map(|kind| concat_dir(kind, &group));
        for directory in &directories {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("kept.mkv"), b"kept").unwrap();
            std::fs::write(directory.join("retired.mkv"), b"retired").unwrap();
            std::fs::write(directory.join("pnmpeg_compat_abc.mp4"), b"derived").unwrap();
        }

        let manifest = AssetManifest {
            revision: "r".to_string(),
            entries: vec![
                entry(AssetKind::Intro, &group, "kept.mkv", "x"),
                entry(AssetKind::Outro, &group, "kept.mkv", "x"),
            ],
        };
        let removed = prune_concat(&manifest);
        for directory in &directories {
            assert!(directory.join("kept.mkv").exists());
            assert!(!directory.join("retired.mkv").exists());
            // The generated variant was derived from a set that has just changed, and regenerates
            // on demand; keeping it would concat from a file that is gone.
            assert!(!directory.join("pnmpeg_compat_abc.mp4").exists());
        }
        assert_eq!(removed.len(), 4);

        // A group the corpus dropped entirely takes its directory with it.
        prune_concat(&AssetManifest { revision: "r".to_string(), entries: Vec::new() });
        for kind in CONCAT_KINDS {
            assert!(!concat_dir(kind, &group).exists());
            assert!(!concat_group_is_populated(kind, &group));
        }
    }

    // A node must never install bytes that are not what the manifest promised, whatever the reason
    // — that is the whole difference between syncing a corpus and trusting a stream.
    #[test]
    fn writing_refuses_content_that_does_not_match_its_hash() {
        let entry = entry(AssetKind::Font, "main", "a.ttf", "not-the-hash");
        assert!(write_asset(&entry, b"some bytes").is_err());
    }
}
