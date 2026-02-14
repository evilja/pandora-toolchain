use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;
use crate::pnworker::core::{Job, Stage, JobType, Preset};

pub struct JobDb {
    pool: SqlitePool,
}

impl JobDb {
    pub async fn new() -> Result<Self, sqlx::Error> {
        let db_path = PathBuf::from("DB").join("DATA.db");

        tokio::fs::create_dir_all("DB").await?;

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
            .await?;

        Ok(Self { pool })
    }

    pub async fn init_schema(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                job_id INTEGER PRIMARY KEY,
                author INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                requested_at INTEGER NOT NULL,
                job_type INTEGER NOT NULL,
                preset_type INTEGER NOT NULL,
                preset_concat INTEGER,
                link TEXT NOT NULL,
                directory TEXT NOT NULL,
                stage INTEGER NOT NULL,
                archived INTEGER DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_jobs_author ON jobs(author);
            CREATE INDEX IF NOT EXISTS idx_jobs_channel ON jobs(channel_id);
            CREATE INDEX IF NOT EXISTS idx_jobs_stage ON jobs(stage);
            CREATE INDEX IF NOT EXISTS idx_jobs_archived ON jobs(archived);
            "#
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn insert_job(&self, job: &Job) -> Result<(), sqlx::Error> {
        let (preset_type, preset_concat) = match job.preset {
            Preset::PseudoLossless(c) => (0, c),
            Preset::Standard(c) => (1, c),
            Preset::Gpu(c) => (2, c),
        };

        sqlx::query(
            r#"
            INSERT INTO jobs (
                job_id, author, channel_id, requested_at, job_type,
                preset_type, preset_concat, link, directory, stage
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(job.job_id as i64)
        .bind(job.author as i64)
        .bind(job.channel_id as i64)
        .bind(job.requested_at.as_secs() as i64)
        .bind(job.job_type as u16 as i64)
        .bind(preset_type)
        .bind(preset_concat)
        .bind(&job.link)
        .bind(job.directory.to_string_lossy().to_string())
        .bind(stage_to_int(job.ready))
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_stage(&self, job_id: u64, stage: Stage) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE jobs
            SET stage = ?
            WHERE job_id = ?
            "#
        )
        .bind(stage_to_int(stage))
        .bind(job_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn archive_job(&self, job_id: u64) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE jobs
            SET archived = 1
            WHERE job_id = ?
            "#
        )
        .bind(job_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_job(&self, job_id: u64) -> Result<Option<JobRow>, sqlx::Error> {
        let row = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, requested_at, job_type,
                   preset_type, preset_concat, link, directory, stage, archived
            FROM jobs
            WHERE job_id = ?
            "#
        )
        .bind(job_id as i64)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn get_active_jobs(&self) -> Result<Vec<JobRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, requested_at, job_type,
                   preset_type, preset_concat, link, directory, stage, archived
            FROM jobs
            WHERE archived = 0
            ORDER BY requested_at ASC
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn get_jobs_by_author(&self, author: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, requested_at, job_type,
                   preset_type, preset_concat, link, directory, stage, archived
            FROM jobs
            WHERE author = ?
            ORDER BY requested_at DESC
            "#
        )
        .bind(author as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }
}

#[derive(sqlx::FromRow, Debug)]
pub struct JobRow {
    pub job_id: i64,
    pub author: i64,
    pub channel_id: i64,
    pub requested_at: i64,
    pub job_type: i64,
    pub preset_type: i64,
    pub preset_concat: Option<i64>,
    pub link: String,
    pub directory: String,
    pub stage: i64,
    pub archived: i64,
}

fn stage_to_int(stage: Stage) -> i64 {
    match stage {
        Stage::Queued => 0,
        Stage::Downloading => 1,
        Stage::Downloaded => 2,
        Stage::Encoding => 3,
        Stage::Encoded => 4,
        Stage::Uploading => 5,
        Stage::Uploaded => 6,
        Stage::Failed => 7,
        Stage::Declined => 8,
    }
}

fn int_to_stage(val: i64) -> Stage {
    match val {
        0 => Stage::Queued,
        1 => Stage::Downloading,
        2 => Stage::Downloaded,
        3 => Stage::Encoding,
        4 => Stage::Encoded,
        5 => Stage::Uploading,
        6 => Stage::Uploaded,
        7 => Stage::Failed,
        8 => Stage::Declined,
        _ => Stage::Failed,
    }
}
