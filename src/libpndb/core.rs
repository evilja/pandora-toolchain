use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;
use crate::pnworker::core::{Job, Stage, Preset};

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
                job_id       INTEGER PRIMARY KEY,
                author       INTEGER NOT NULL,
                channel_id   INTEGER NOT NULL,
                response_id  INTEGER NOT NULL DEFAULT 0,
                requested_at INTEGER NOT NULL,
                job_type     INTEGER NOT NULL,
                preset_type  INTEGER NOT NULL,
                candidates   TEXT,
                link         TEXT NOT NULL,
                directory    TEXT NOT NULL,
                stage        INTEGER NOT NULL,
                archived     INTEGER DEFAULT 0,
                created_at   DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        for idx in [
            "CREATE INDEX IF NOT EXISTS idx_jobs_author   ON jobs(author);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_channel  ON jobs(channel_id);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_stage    ON jobs(stage);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_archived ON jobs(archived);",
        ] {
            sqlx::query(idx).execute(&self.pool).await?;
        }

        Ok(())
    }

    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        // Add response_id if missing (old DBs)
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN response_id INTEGER NOT NULL DEFAULT 0"
        ).await?;

        // Add candidates column if missing
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN candidates TEXT"
        ).await?;

        // Convert old preset_concat integer to group name string.
        // Only known mapping: 1 -> "SomeSubs"
        sqlx::query(
            r#"
            UPDATE jobs
            SET candidates = CASE preset_concat
                WHEN 1 THEN 'SomeSubs'
                ELSE NULL
            END
            WHERE candidates IS NULL AND preset_concat IS NOT NULL
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn add_column_if_missing(&self, alter_sql: &str) -> Result<(), sqlx::Error> {
        sqlx::query(alter_sql)
            .execute(&self.pool)
            .await
            .or_else(|e| {
                if e.to_string().contains("duplicate column name") {
                    Ok(Default::default())
                } else {
                    Err(e)
                }
            })?;
        Ok(())
    }

    pub async fn insert_job(&self, job: &Job) -> Result<(), sqlx::Error> {
        let (preset_type, candidates) = match &job.preset {
            Preset::PseudoLossless(c) => (0i64, candidates_to_db(c)),
            Preset::Standard(c)       => (1i64, candidates_to_db(c)),
            Preset::Gpu(c)            => (2i64, candidates_to_db(c)),
            Preset::Dummy(c)          => (3i64, candidates_to_db(c)),
        };

        sqlx::query(
            r#"
            INSERT INTO jobs (
                job_id, author, channel_id, response_id, requested_at,
                job_type, preset_type, candidates, link, directory, stage
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(job.job_id as i64)
        .bind(job.author as i64)
        .bind(job.channel_id as i64)
        .bind(job.response_id as i64)
        .bind(job.requested_at.as_secs() as i64)
        .bind(job.job_type as u16 as i64)
        .bind(preset_type)
        .bind(candidates)
        .bind(&job.torrent.get())
        .bind(job.directory.to_string_lossy().to_string())
        .bind(stage_to_int(job.ready))
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_response_id(&self, job_id: u64, response_id: u64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET response_id = ? WHERE job_id = ?")
            .bind(response_id as i64)
            .bind(job_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_stage(&self, job_id: u64, stage: Stage) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET stage = ? WHERE job_id = ?")
            .bind(stage_to_int(stage))
            .bind(job_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn archive_job(&self, job_id: u64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET archived = 1 WHERE job_id = ?")
            .bind(job_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_job(&self, job_id: u64) -> Result<Option<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived
            FROM jobs WHERE job_id = ?
            "#,
        )
        .bind(job_id as i64)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn get_active_jobs(&self) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived
            FROM jobs WHERE archived = 0 ORDER BY requested_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_jobs_by_author(&self, author: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived
            FROM jobs WHERE author = ? ORDER BY requested_at DESC
            "#,
        )
        .bind(author as i64)
        .fetch_all(&self.pool)
        .await
    }
}

#[derive(sqlx::FromRow, Debug)]
pub struct JobRow {
    pub job_id:       i64,
    pub author:       i64,
    pub channel_id:   i64,
    pub response_id:  i64,
    pub requested_at: i64,
    pub job_type:     i64,
    pub preset_type:  i64,
    pub candidates:   Option<String>,
    pub link:         String,
    pub directory:    String,
    pub stage:        i64,
    pub archived:     i64,
}

impl JobRow {
    pub fn candidates_as_vec(&self) -> Option<Vec<String>> {
        self.candidates.as_ref().map(|s| {
            s.split(',').map(|p| p.to_string()).collect()
        })
    }
}

fn candidates_to_db(candidates: &Option<Vec<String>>) -> Option<String> {
    candidates.as_ref().map(|v| v.join(","))
}

fn stage_to_int(stage: Stage) -> i64 {
    match stage {
        Stage::Queued      => 0,
        Stage::Downloading => 1,
        Stage::Downloaded  => 2,
        Stage::Encoding    => 3,
        Stage::Encoded     => 4,
        Stage::Uploading   => 5,
        Stage::Uploaded    => 6,
        Stage::Failed      => 7,
        Stage::Declined    => 8,
        Stage::Cancelled   => 9,
    }
}
