use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
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

        sqlx::query("PRAGMA journal_mode=WAL;").execute(&pool).await?;

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
                progress     TEXT,
                uploaded_links TEXT,
                acix_pending TEXT,
                server_id    INTEGER,
                worker       TEXT DEFAULT 'que-main',
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

        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN progress TEXT"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN uploaded_links TEXT"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN acix_pending TEXT"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN server_id INTEGER"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN worker TEXT DEFAULT 'que-main'"
        ).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_server ON jobs(server_id);")
            .execute(&self.pool)
            .await?;

        if self.column_exists("jobs", "preset_concat").await? {
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
        }

        Ok(())
    }

    async fn column_exists(&self, table: &str, column: &str) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query(&format!("PRAGMA table_info({})", table))
            .fetch_all(&self.pool)
            .await?;
        for row in rows {
            let name: String = row.try_get("name")?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
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
            Preset::Copy              => (4i64, None),
        };
        let link = job
            .display_link
            .clone()
            .unwrap_or_else(|| job.torrent.get());

        sqlx::query(
            r#"
            INSERT INTO jobs (
                job_id, author, channel_id, response_id, requested_at,
                job_type, preset_type, candidates, link, directory, stage, server_id, worker
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(job_id) DO UPDATE SET
                author = excluded.author,
                channel_id = excluded.channel_id,
                response_id = excluded.response_id,
                requested_at = excluded.requested_at,
                job_type = excluded.job_type,
                preset_type = excluded.preset_type,
                candidates = excluded.candidates,
                link = excluded.link,
                directory = excluded.directory,
                stage = excluded.stage,
                server_id = excluded.server_id,
                worker = excluded.worker,
                archived = 0
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
        .bind(link)
        .bind(job.directory.to_string_lossy().to_string())
        .bind(stage_to_int(job.ready))
        .bind(job.server_id.map(|id| id as i64))
        .bind(&job.worker)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_worker(&self, job_id: u64, worker: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET worker = ? WHERE job_id = ?")
            .bind(worker)
            .bind(job_id as i64)
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

    pub async fn update_progress(&self, job_id: u64, progress: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET progress = ? WHERE job_id = ?")
            .bind(progress)
            .bind(job_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_links(&self, job_id: u64, links: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET uploaded_links = ? WHERE job_id = ?")
            .bind(links)
            .bind(job_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_acix_pending(&self, job_id: u64, pending: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET acix_pending = ? WHERE job_id = ?")
            .bind(pending)
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

    pub async fn fail_stale_active(&self) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE jobs SET stage = 7 WHERE archived = 0 AND stage NOT IN (6, 7, 8, 9)"
        )
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn get_job(&self, job_id: u64) -> Result<Option<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived,
                   progress, uploaded_links, acix_pending, server_id, COALESCE(worker, 'que-main') AS worker
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
                   job_type, preset_type, candidates, link, directory, stage, archived,
                   progress, uploaded_links, acix_pending, server_id, COALESCE(worker, 'que-main') AS worker
            FROM jobs WHERE archived = 0 ORDER BY requested_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_ongoing_jobs(&self) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived,
                   progress, uploaded_links, acix_pending, server_id, COALESCE(worker, 'que-main') AS worker
            FROM jobs WHERE archived = 0 AND stage NOT IN (6, 7, 8, 9) ORDER BY requested_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_recent_jobs(&self, limit: i64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived,
                   progress, uploaded_links, acix_pending, server_id, COALESCE(worker, 'que-main') AS worker
            FROM jobs ORDER BY requested_at DESC LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_jobs_by_author(&self, author: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, author, channel_id, response_id, requested_at,
                   job_type, preset_type, candidates, link, directory, stage, archived,
                   progress, uploaded_links, acix_pending, server_id, COALESCE(worker, 'que-main') AS worker
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
    pub progress:        Option<String>,
    pub uploaded_links:  Option<String>,
    pub acix_pending:    Option<String>,
    pub server_id:       Option<i64>,
    pub worker:          String,
}

impl JobRow {
    pub fn candidates_as_vec(&self) -> Option<Vec<String>> {
        self.candidates.as_ref().map(|s| {
            s.split(',').map(|p| p.to_string()).collect()
        })
    }
}

fn serialize_id_as_str<S>(id: &i64, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_str(&id.to_string())
}

#[derive(serde::Serialize, Debug)]
pub struct JobStatus {
    #[serde(serialize_with = "serialize_id_as_str")]
    pub job_id:     i64,
    pub author:     i64,
    pub channel_id: i64,
    pub server_id:  Option<i64>,
    pub job_type:   String,
    pub preset:     String,
    pub stage:      String,
    pub worker:     String,
    pub link:       String,
    pub archived:   bool,
    pub progress:   Option<serde_json::Value>,
    pub links:      Option<serde_json::Value>,
    pub acix:       Option<serde_json::Value>,
}

impl JobStatus {
    pub fn from_row(row: &JobRow) -> Self {
        Self {
            job_id:     row.job_id,
            author:     row.author,
            channel_id: row.channel_id,
            server_id:  row.server_id,
            job_type:   job_type_label(row.job_type).to_string(),
            preset:     preset_label(row.preset_type).to_string(),
            stage:      stage_label(row.stage).to_string(),
            worker:     row.worker.clone(),
            link:       row.link.clone(),
            archived:   row.archived != 0,
            progress:   row.progress.as_deref().and_then(|s| serde_json::from_str(s).ok()),
            links:      row.uploaded_links.as_deref().and_then(|s| serde_json::from_str(s).ok()),
            acix:       row.acix_pending.as_deref().and_then(|s| serde_json::from_str(s).ok()),
        }
    }
}

pub fn stage_label(stage: i64) -> &'static str {
    match stage {
        0  => "Queued",
        1  => "Downloading",
        2  => "Downloaded",
        3  => "Encoding",
        4  => "Encoded",
        5  => "Uploading",
        6  => "Uploaded",
        7  => "Failed",
        8  => "Declined",
        9  => "Cancelled",
        20 => "Probing",
        21 => "Probed",
        _  => "Unknown",
    }
}

pub fn job_type_label(job_type: i64) -> &'static str {
    match job_type {
        1 => "Encode",
        2 => "Cancel",
        3 => "Hearts",
        4 => "GitSync",
        5 => "Probe",
        6 => "Pancode",
        7 => "Scrape",
        8 => "Backup",
        9 => "BackupAll",
        10 => "Keycode",
        11 => "GitQuery",
        13 => "Preview",
        14 => "Studio",
        15 => "StudioPreview",
        _ => "Unknown",
    }
}

pub fn preset_label(preset_type: i64) -> &'static str {
    match preset_type {
        0 => "PseudoLossless",
        1 => "Standard",
        2 => "Gpu",
        3 => "Dummy",
        4 => "Copy",
        _ => "Unknown",
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
        Stage::Probing     => 20,
        Stage::Probed      => 21,
    }
}
