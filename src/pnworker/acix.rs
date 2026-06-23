use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::libacix::{AnimeCix, MixedUpload};
use crate::libpndb::core::JobDb;
use crate::pnworker::core::AcixPublish;

#[derive(Clone, Serialize, Deserialize)]
pub struct AcixPending {
    pub status: String,
    pub acix: AcixPublish,
    pub drive: String,
}

pub async fn run_publish(acix: &AcixPublish, drive: &str) -> Result<Value, String> {
    let client = AnimeCix::from_env()?;
    let hit = client
        .resolve_by_mal_id(&acix.name, acix.mal_id)
        .await?
        .ok_or_else(|| format!("no AnimeciX match for mal_id {} ({})", acix.mal_id, acix.name))?;
    let up = MixedUpload::new(
        acix.extra.clone(),
        drive.to_string(),
        acix.template,
        hit.acix_id,
        acix.season_num,
        acix.episode_num,
    );
    client.multishare_mixed(&up).await
}

pub async fn confirm_acix(db: &JobDb, job_id: u64) -> Result<Value, String> {
    let row = db
        .get_job(job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such job".to_string())?;
    let pending_json = row
        .acix_pending
        .ok_or_else(|| "no pending AnimeciX publish for this job".to_string())?;
    let mut pending: AcixPending = serde_json::from_str(&pending_json).map_err(|e| e.to_string())?;
    if pending.status == "published" {
        return Err("this job was already published to AnimeciX".to_string());
    }
    match run_publish(&pending.acix, &pending.drive).await {
        Ok(v) => {
            pending.status = "published".to_string();
            if let Ok(j) = serde_json::to_string(&pending) {
                db.set_acix_pending(job_id, &j).await.ok();
            }
            Ok(v)
        }
        Err(e) => {
            pending.status = "failed".to_string();
            if let Ok(j) = serde_json::to_string(&pending) {
                db.set_acix_pending(job_id, &j).await.ok();
            }
            Err(e)
        }
    }
}
