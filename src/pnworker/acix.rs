use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lib::db::core::JobDb;
use crate::lib::http::acix::{AnimeCix, MixedUpload};
use crate::pnworker::core::{AcixCredits, AcixPublish};

const PUBLIC_LINK_KEYS: &[&str] = &["drive", "byse", "lulustream", "voe"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PublishState {
    Pending,
    Published,
    Failed,
}

#[derive(Clone, Default)]
pub struct CreditOverrides {
    extra: Option<String>,
    tl: Option<Option<String>>,
    tlc: Option<Option<String>>,
    ts: Option<Option<String>>,
    qc: Option<Option<String>>,
    template: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcixResetScope {
    Multiple,
    Multishare,
    Both,
}

impl AcixResetScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "multiple" => Some(Self::Multiple),
            "multishare" => Some(Self::Multishare),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Multiple => "multiple",
            Self::Multishare => "multishare",
            Self::Both => "both",
        }
    }
}

impl CreditOverrides {
    pub fn from_values(
        extra: Option<String>,
        tl: Option<String>,
        tlc: Option<String>,
        ts: Option<String>,
        qc: Option<String>,
    ) -> Result<Self, String> {
        let has_roles = [&tl, &tlc, &ts, &qc].iter().any(|value| value.is_some());
        if extra.is_some() && has_roles {
            return Err("`extra` cannot be combined with `tl`, `tlc`, `ts`, or `qc`".to_string());
        }
        Ok(Self {
            extra: extra.map(|value| normalize_extra_override(&value)),
            tl: role_override(tl),
            tlc: role_override(tlc),
            ts: role_override(ts),
            qc: role_override(qc),
            template: None,
        })
    }

    // Publishing under a fansub other than the one queued at upload time is the same kind of
    // pre-publish edit as a credit line: it rewrites the queued record, so it is refused once either
    // half has reached AnimeciX rather than sending the second half under a different group.
    pub fn with_template(mut self, template: i64) -> Self {
        self.template = Some(template);
        self
    }

    fn has_any(&self) -> bool {
        self.extra.is_some() || self.template.is_some() || self.has_role_override()
    }

    fn has_role_override(&self) -> bool {
        self.tl.is_some() || self.tlc.is_some() || self.ts.is_some() || self.qc.is_some()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AcixPending {
    pub status: String,
    pub acix: AcixPublish,
    pub drive: String,
    #[serde(default)]
    multishare_status: Option<PublishState>,
    #[serde(default)]
    multiple_status: Option<PublishState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    multishare_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    multiple_error: Option<String>,
}

impl AcixPending {
    pub fn new(acix: AcixPublish, drive: String) -> Self {
        Self {
            status: "pending".to_string(),
            acix,
            drive,
            multishare_status: Some(PublishState::Pending),
            multiple_status: Some(PublishState::Pending),
            multishare_error: None,
            multiple_error: None,
        }
    }

    // Legacy records used `status=published` only after multishare succeeded.
    // Treat that half as complete so upgrading never republishes it.
    fn upgrade_legacy_state(&mut self) {
        if self.multishare_status.is_none() {
            self.multishare_status = Some(if self.status == "published" {
                PublishState::Published
            } else {
                PublishState::Pending
            });
        }
        if self.multiple_status.is_none() {
            self.multiple_status = Some(PublishState::Pending);
        }
        self.refresh_status();
    }

    fn refresh_status(&mut self) {
        let multishare = self.multishare_status.as_ref().unwrap_or(&PublishState::Pending);
        let multiple = self.multiple_status.as_ref().unwrap_or(&PublishState::Pending);
        self.status = if multishare == &PublishState::Published && multiple == &PublishState::Published {
            "published"
        } else if multishare == &PublishState::Published || multiple == &PublishState::Published {
            "partial"
        } else if multishare == &PublishState::Failed || multiple == &PublishState::Failed {
            "failed"
        } else {
            "pending"
        }.to_string();
    }

    fn fully_published(&self) -> bool {
        self.multishare_status == Some(PublishState::Published)
            && self.multiple_status == Some(PublishState::Published)
    }

    fn reset_publish_state(&mut self, scope: AcixResetScope) {
        if matches!(scope, AcixResetScope::Multishare | AcixResetScope::Both) {
            self.multishare_status = Some(PublishState::Pending);
            self.multishare_error = None;
        }
        if matches!(scope, AcixResetScope::Multiple | AcixResetScope::Both) {
            self.multiple_status = Some(PublishState::Pending);
            self.multiple_error = None;
        }
        self.refresh_status();
    }

    fn apply_overrides(&mut self, overrides: &CreditOverrides) -> Result<(), String> {
        if !overrides.has_any() {
            return Ok(());
        }
        if self.multishare_status == Some(PublishState::Published)
            || self.multiple_status == Some(PublishState::Published)
        {
            return Err("credit and fansub overrides cannot be changed after a partial AnimeciX publish; retry without overrides".to_string());
        }
        if let Some(template) = overrides.template {
            self.acix.template = template;
        }
        if let Some(extra) = &overrides.extra {
            self.acix.extra = extra.clone();
            self.acix.credits = None;
            return Ok(());
        }
        // A fansub-only override leaves the queued credits alone, so freeform ones are never parsed
        // and never refused for a publish that was not editing them in the first place.
        if !overrides.has_role_override() {
            return Ok(());
        }

        let mut credits = self.acix.credits.clone()
            .or_else(|| parse_legacy_credits(&self.acix.extra))
            .ok_or_else(|| "queued AnimeciX credits are freeform; use `extra` to replace them".to_string())?;
        apply_role_override(&mut credits.tl, &overrides.tl);
        apply_role_override(&mut credits.tlc, &overrides.tlc);
        apply_role_override(&mut credits.ts, &overrides.ts);
        apply_role_override(&mut credits.qc, &overrides.qc);
        self.acix.extra = credits.extra();
        self.acix.credits = Some(credits);
        Ok(())
    }
}

fn normalize_extra_override(value: &str) -> String {
    let value = value.trim();
    if value == "-" { String::new() } else { value.to_string() }
}

fn role_override(value: Option<String>) -> Option<Option<String>> {
    value.map(|value| {
        let value = value.trim();
        if value.is_empty() || value == "-" || value == "---" {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn apply_role_override(target: &mut Option<String>, value: &Option<Option<String>>) {
    if let Some(value) = value {
        *target = value.clone();
    }
}

fn parse_legacy_credits(extra: &str) -> Option<AcixCredits> {
    let markers = [
        ("Çeviri: ", 0usize),
        ("Redaktör: ", 1usize),
        ("Tipset: ", 2usize),
        ("Kalite Kontrol: ", 3usize),
    ];
    if extra.trim().is_empty() {
        return Some(AcixCredits::default());
    }
    let mut found = markers.iter()
        .filter_map(|(marker, index)| extra.find(marker).map(|position| (position, *marker, *index)))
        .collect::<Vec<_>>();
    found.sort_by_key(|(position, _, _)| *position);
    if found.first().map(|(position, _, _)| *position) != Some(0) {
        return None;
    }
    if found.windows(2).any(|pair| pair[0].2 >= pair[1].2) {
        return None;
    }

    let mut credits = AcixCredits::default();
    for (found_index, (position, marker, credit_index)) in found.iter().enumerate() {
        let start = position + marker.len();
        let end = found.get(found_index + 1)
            .map(|(next_position, _, _)| *next_position)
            .unwrap_or(extra.len());
        let value = extra[start..end].trim();
        let value = if value.is_empty() || value == "---" { None } else { Some(value.to_string()) };
        match credit_index {
            0 => credits.tl = value,
            1 => credits.tlc = value,
            2 => credits.ts = value,
            3 => credits.qc = value,
            _ => {}
        }
    }
    Some(credits)
}

fn public_uploaded_links(drive: &str, uploaded_links: Option<&str>) -> Result<Vec<String>, String> {
    let mut links = Vec::new();
    push_public_link(&mut links, drive);
    if let Some(json) = uploaded_links {
        let value: Value = serde_json::from_str(json)
            .map_err(|e| format!("uploaded links JSON is invalid: {}", e))?;
        for key in PUBLIC_LINK_KEYS {
            if let Some(link) = value.get(*key).and_then(|item| item.as_str()) {
                push_public_link(&mut links, link);
            }
        }
    }
    if links.is_empty() {
        return Err("job has no public upload links for AnimeciX multiple".to_string());
    }
    Ok(links)
}

fn push_public_link(links: &mut Vec<String>, link: &str) {
    let link = link.trim();
    if (link.starts_with("https://") || link.starts_with("http://"))
        && !links.iter().any(|existing| existing == link)
    {
        links.push(link.to_string());
    }
}

async fn persist_pending(db: &JobDb, job_id: u64, pending: &mut AcixPending) -> Result<(), String> {
    pending.refresh_status();
    let json = serde_json::to_string(pending).map_err(|e| e.to_string())?;
    db.set_acix_pending(job_id, &json).await.map_err(|e| e.to_string())
}

fn mark_shared_failure(pending: &mut AcixPending, error: &str) {
    if pending.multishare_status != Some(PublishState::Published) {
        pending.multishare_status = Some(PublishState::Failed);
        pending.multishare_error = Some(error.to_string());
    }
    if pending.multiple_status != Some(PublishState::Published) {
        pending.multiple_status = Some(PublishState::Failed);
        pending.multiple_error = Some(error.to_string());
    }
}

pub async fn unpublish_acix(
    db: &JobDb,
    job_id: u64,
    scope: AcixResetScope,
) -> Result<Value, String> {
    let row = db
        .get_job(job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such job".to_string())?;
    let pending_json = row.acix_pending.as_deref()
        .ok_or_else(|| "no AnimeciX publish state for this job".to_string())?;
    let mut pending: AcixPending = serde_json::from_str(pending_json).map_err(|e| e.to_string())?;
    pending.upgrade_legacy_state();
    pending.reset_publish_state(scope);
    persist_pending(db, job_id, &mut pending).await?;
    Ok(serde_json::json!({
        "status": pending.status,
        "scope": scope.as_str(),
        "remote_deleted": false,
    }))
}

pub async fn confirm_acix(db: &JobDb, job_id: u64) -> Result<Value, String> {
    confirm_acix_with_overrides(db, job_id, CreditOverrides::default()).await
}

pub async fn confirm_acix_with_overrides(
    db: &JobDb,
    job_id: u64,
    overrides: CreditOverrides,
) -> Result<Value, String> {
    let row = db
        .get_job(job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such job".to_string())?;
    let pending_json = row.acix_pending.as_deref()
        .ok_or_else(|| "no pending AnimeciX publish for this job".to_string())?;
    let mut pending: AcixPending = serde_json::from_str(pending_json).map_err(|e| e.to_string())?;
    pending.upgrade_legacy_state();
    if pending.fully_published() {
        return Err("this job was already published to AnimeciX multishare and multiple".to_string());
    }
    if overrides.has_any() {
        pending.apply_overrides(&overrides)?;
        persist_pending(db, job_id, &mut pending).await?;
    }

    let client = match AnimeCix::from_env() {
        Ok(client) => client,
        Err(e) => {
            mark_shared_failure(&mut pending, &e);
            persist_pending(db, job_id, &mut pending).await?;
            return Err(e);
        }
    };
    let hit = match client.resolve_by_mal_id(&pending.acix.name, pending.acix.mal_id).await {
        Ok(Some(hit)) => hit,
        Ok(None) => {
            let e = format!(
                "no AnimeciX match for mal_id {} ({})",
                pending.acix.mal_id, pending.acix.name,
            );
            mark_shared_failure(&mut pending, &e);
            persist_pending(db, job_id, &mut pending).await?;
            return Err(e);
        }
        Err(e) => {
            mark_shared_failure(&mut pending, &e);
            persist_pending(db, job_id, &mut pending).await?;
            return Err(e);
        }
    };
    let up = MixedUpload::new(
        pending.acix.extra.clone(),
        pending.drive.clone(),
        pending.acix.template,
        hit.acix_id,
        pending.acix.season_num,
        pending.acix.episode_num,
    );

    let mut errors = Vec::new();
    let multishare_result = if pending.multishare_status == Some(PublishState::Published) {
        serde_json::json!({ "skipped": "already published" })
    } else {
        match client.multishare_mixed(&up).await {
            Ok(value) => {
                pending.multishare_status = Some(PublishState::Published);
                pending.multishare_error = None;
                persist_pending(db, job_id, &mut pending).await?;
                value
            }
            Err(e) => {
                pending.multishare_status = Some(PublishState::Failed);
                pending.multishare_error = Some(e.clone());
                errors.push(format!("multishare: {}", e));
                persist_pending(db, job_id, &mut pending).await?;
                Value::Null
            }
        }
    };

    let multiple_result = if pending.multiple_status == Some(PublishState::Published) {
        serde_json::json!({ "skipped": "already published" })
    } else {
        match public_uploaded_links(&pending.drive, row.uploaded_links.as_deref()) {
            Ok(links) => match client.multiple(&up, &links).await {
                Ok(value) => {
                    pending.multiple_status = Some(PublishState::Published);
                    pending.multiple_error = None;
                    persist_pending(db, job_id, &mut pending).await?;
                    value
                }
                Err(e) => {
                    pending.multiple_status = Some(PublishState::Failed);
                    pending.multiple_error = Some(e.clone());
                    errors.push(format!("multiple: {}", e));
                    persist_pending(db, job_id, &mut pending).await?;
                    Value::Null
                }
            },
            Err(e) => {
                pending.multiple_status = Some(PublishState::Failed);
                pending.multiple_error = Some(e.clone());
                errors.push(format!("multiple: {}", e));
                persist_pending(db, job_id, &mut pending).await?;
                Value::Null
            }
        }
    };

    let result = serde_json::json!({
        "status": pending.status,
        "multishare": multishare_result,
        "multiple": multiple_result,
    });
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(format!("AnimeciX publish status `{}` — {}", pending.status, errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::{public_uploaded_links, AcixPending, AcixResetScope, CreditOverrides, PublishState};
    use crate::pnworker::core::{AcixCredits, AcixPublish};

    fn acix() -> AcixPublish {
        let credits = AcixCredits {
            tl: Some("Translator".to_string()),
            tlc: Some("Editor".to_string()),
            ts: None,
            qc: None,
        };
        AcixPublish {
            name: "Anime".to_string(),
            mal_id: 1,
            season_num: Some(1),
            episode_num: Some(2),
            template: 50,
            extra: credits.extra(),
            credits: Some(credits),
        }
    }

    #[test]
    fn extracts_ordered_public_links_and_ignores_internal_fields() {
        let json = serde_json::json!({
            "drive": "https://drive.example/video",
            "byse": "https://byse.example/video",
            "lulustream": null,
            "voe": "https://voe.example/video",
            "drive_file_id": "internal-id",
            "warnings": ["warning"]
        }).to_string();
        assert_eq!(
            public_uploaded_links("https://drive.example/video", Some(&json)).unwrap(),
            vec![
                "https://drive.example/video".to_string(),
                "https://byse.example/video".to_string(),
                "https://voe.example/video".to_string(),
            ]
        );
    }

    #[test]
    fn legacy_published_state_retries_only_multiple() {
        let legacy = serde_json::json!({
            "status": "published",
            "acix": {
                "name": "Anime",
                "mal_id": 1,
                "season_num": 1,
                "episode_num": 2,
                "template": 50,
                "extra": "Çeviri: Translator Redaktör: Editor"
            },
            "drive": "https://drive.example/video"
        });
        let mut pending: AcixPending = serde_json::from_value(legacy).unwrap();
        pending.upgrade_legacy_state();
        assert_eq!(pending.multishare_status, Some(PublishState::Published));
        assert_eq!(pending.multiple_status, Some(PublishState::Pending));
        assert_eq!(pending.status, "partial");
    }

    #[test]
    fn aggregate_status_tracks_partial_and_complete_publish() {
        let mut pending = AcixPending::new(acix(), "https://drive.example/video".to_string());
        pending.multishare_status = Some(PublishState::Published);
        pending.refresh_status();
        assert_eq!(pending.status, "partial");
        pending.multiple_status = Some(PublishState::Published);
        pending.refresh_status();
        assert_eq!(pending.status, "published");
        assert!(pending.fully_published());
    }

    #[test]
    fn role_overrides_keep_replace_and_clear_credits() {
        let mut pending = AcixPending::new(acix(), "https://drive.example/video".to_string());
        let overrides = CreditOverrides::from_values(
            None,
            None,
            Some("New Editor".to_string()),
            Some("Typesetter".to_string()),
            Some("-".to_string()),
        ).unwrap();
        pending.apply_overrides(&overrides).unwrap();
        assert_eq!(pending.acix.extra, "Translator & New Editor & Typesetter");
    }

    #[test]
    fn legacy_labeled_credits_can_be_overridden() {
        let mut publish = acix();
        publish.extra = "Çeviri: Translator Redaktör: Editor Tipset: Typesetter Kalite Kontrol: QC".to_string();
        publish.credits = None;
        let mut pending = AcixPending::new(publish, "https://drive.example/video".to_string());
        let overrides = CreditOverrides::from_values(
            None,
            Some("New Translator".to_string()),
            None,
            None,
            Some("-".to_string()),
        ).unwrap();
        pending.apply_overrides(&overrides).unwrap();
        assert_eq!(pending.acix.extra, "New Translator & Editor & Typesetter");
    }

    #[test]
    fn reset_scope_only_reopens_selected_publish_half() {
        let mut pending = AcixPending::new(acix(), "https://drive.example/video".to_string());
        pending.multishare_status = Some(PublishState::Published);
        pending.multiple_status = Some(PublishState::Published);
        pending.multishare_error = Some("old multishare error".to_string());
        pending.multiple_error = Some("old multiple error".to_string());

        pending.reset_publish_state(AcixResetScope::Multiple);
        assert_eq!(pending.multishare_status, Some(PublishState::Published));
        assert_eq!(pending.multiple_status, Some(PublishState::Pending));
        assert!(pending.multishare_error.is_some());
        assert!(pending.multiple_error.is_none());
        assert_eq!(pending.status, "partial");

        pending.reset_publish_state(AcixResetScope::Both);
        assert_eq!(pending.multishare_status, Some(PublishState::Pending));
        assert_eq!(pending.multiple_status, Some(PublishState::Pending));
        assert!(pending.multishare_error.is_none());
        assert_eq!(pending.status, "pending");
    }

    #[test]
    fn freeform_extra_is_exclusive_and_partial_publish_locks_overrides() {
        assert!(CreditOverrides::from_values(
            Some("Custom".to_string()),
            Some("Translator".to_string()),
            None,
            None,
            None,
        ).is_err());

        let mut pending = AcixPending::new(acix(), "https://drive.example/video".to_string());
        pending.multishare_status = Some(PublishState::Published);
        let overrides = CreditOverrides::from_values(
            Some("Custom credits".to_string()),
            None,
            None,
            None,
            None,
        ).unwrap();
        assert!(pending.apply_overrides(&overrides).is_err());
    }

    #[test]
    fn a_fansub_override_republishes_under_another_template_without_touching_credits() {
        let mut publish = acix();
        publish.extra = "freeform credits".to_string();
        publish.credits = None;
        let queued = publish.template;
        let mut pending = AcixPending::new(publish, "https://drive.example/video".to_string());

        let overrides = CreditOverrides::from_values(None, None, None, None, None)
            .unwrap()
            .with_template(queued + 1);
        pending.apply_overrides(&overrides).unwrap();
        assert_eq!(pending.acix.template, queued + 1);
        // Freeform credits are only refused when a role override actually asked to edit them.
        assert_eq!(pending.acix.extra, "freeform credits");

        pending.multiple_status = Some(PublishState::Published);
        assert!(pending.apply_overrides(&overrides).is_err());
    }
}
