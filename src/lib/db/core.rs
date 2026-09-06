use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;
use crate::pnworker::core::{Job, Stage, Preset};

// Every read of the table selects the same projection into `JobRow`, and it has to keep selecting
// the same one: `worker` was added after rows already existed, so a row written before it must
// still answer with the queue's own name. Five copies of that list is five places to forget a
// column in, so the reads name only what distinguishes them. `concat!` keeps them `&'static str`.
macro_rules! job_query {
    ($tail:literal) => {
        concat!(
            "SELECT job_id, author, channel_id, response_id, requested_at, ",
            "started_at, ended_at, cancel_reason, ",
            "job_type, preset_type, preset_name, candidates, outro, link, directory, stage, archived, ",
            "progress, uploaded_links, acix_pending, server_id, episode, ",
            "COALESCE(worker, 'que-main') AS worker FROM jobs ",
            $tail
        )
    };
}

#[derive(Clone)]
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
                started_at   INTEGER,
                ended_at     INTEGER,
                cancel_reason TEXT,
                job_type     INTEGER NOT NULL,
                preset_type  INTEGER NOT NULL,
                candidates   TEXT,
                outro        TEXT,
                link         TEXT NOT NULL,
                directory    TEXT NOT NULL,
                stage        INTEGER NOT NULL,
                archived     INTEGER DEFAULT 0,
                progress     TEXT,
                uploaded_links TEXT,
                acix_pending TEXT,
                server_id    INTEGER,
                episode      INTEGER,
                worker       TEXT DEFAULT 'que-main',
                created_at   DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Who submitted a job over HTTP, in a table of its own rather than a column on `jobs`.
        // The API knows the submitter the moment it hands the job to the queue, but the row is
        // written later by the worker — and for a forwarded or re-queued job, written again — so a
        // column would be a race with an `ON CONFLICT` clause that resets it. `author` cannot serve
        // either: every API job carries the one configured `api_author_id`.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS job_owners (
                job_id   INTEGER PRIMARY KEY,
                identity TEXT NOT NULL,
                owned_at INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        for idx in [
            "CREATE INDEX IF NOT EXISTS idx_jobs_author   ON jobs(author);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_channel  ON jobs(channel_id);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_stage    ON jobs(stage);",
            "CREATE INDEX IF NOT EXISTS idx_job_owners_identity ON job_owners(identity);",
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

        // The outro folder. `candidates` held the intro before outros existed and goes on holding
        // it, so an old row reads back as a job with an intro and no outro, which is what it was.
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN outro TEXT"
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
        // Which episode of the channel's attached anime this job encoded, for `/smartlist`. Rows
        // written before this column existed keep answering from `acix_pending` — see
        // `JobRow::episode_number` — so nothing is backfilled here.
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN episode INTEGER"
        ).await?;
        // A preset that exists only as a file has no discriminant to be recognised by later, so
        // the name is stored beside the type. Only `Preset::Named` needs it; it is written for
        // every preset because a column that is populated sometimes is one nobody trusts.
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN preset_name TEXT"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN started_at INTEGER"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN ended_at INTEGER"
        ).await?;
        self.add_column_if_missing(
            "ALTER TABLE jobs ADD COLUMN cancel_reason TEXT"
        ).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_jobs_server ON jobs(server_id);")
            .execute(&self.pool)
            .await?;

        // Every job list is ordered by `requested_at` and nothing indexed it, so the console's
        // polling read scanned and sorted the whole table — including the archived rows it then
        // discards — to hand back fifty jobs. `requested_at` never changes after the insert and
        // `archived` changes once, so neither index costs anything on the progress updates that
        // make up almost all of this table's writes. The composite leads with `archived`, which
        // makes the single-column index on it redundant.
        for idx in [
            "CREATE INDEX IF NOT EXISTS idx_jobs_requested ON jobs(requested_at);",
            "CREATE INDEX IF NOT EXISTS idx_jobs_archived_requested ON jobs(archived, requested_at);",
            "DROP INDEX IF EXISTS idx_jobs_archived;",
        ] {
            sqlx::query(idx).execute(&self.pool).await?;
        }

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
        let preset_type = match &job.preset {
            Preset::PseudoLossless(_) => 0i64,
            Preset::Standard(_)       => 1i64,
            Preset::Gpu(_)            => 2i64,
            Preset::Dummy(_)          => 3i64,
            Preset::Copy              => 4i64,
            Preset::VerySlow(_)       => 5i64,
            Preset::Hd720(_)          => 6i64,
            Preset::Sd480(_)          => 7i64,
            Preset::Av1(_)            => 8i64,
            // Every discriminant above names a table compiled into the binary. This one does not,
            // so the row carries the preset's name in `preset_name` and this number means only
            // "look there".
            Preset::Named(_, _)       => 9i64,
        };
        // `candidates` is the intro folder, under the name the column has always had; the outro
        // folder is its own column rather than a second value packed into it, because the console
        // reads this one back and a packed pair would only be a format nobody documented.
        let candidates = concat_to_db(&job.preset.concat().intro);
        let outro = concat_to_db(&job.preset.concat().outro);
        let preset_name = job.preset.name();
        // Smartcode names the episode when it queues the job; an AnimeciX record carries it for the
        // other paths that know one. A job that is not an episode of anything stores nothing.
        let episode = job
            .smartcode_drive_name
            .as_ref()
            .map(|name| name.episode as i64)
            .or_else(|| job.acix.as_ref().and_then(|acix| acix.episode_num));
        let link = job
            .display_link
            .clone()
            .unwrap_or_else(|| job.torrent.get());

        sqlx::query(
            r#"
            INSERT INTO jobs (
                job_id, author, channel_id, response_id, requested_at,
                job_type, preset_type, preset_name, candidates, outro, link, directory, stage, server_id, episode, worker
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(job_id) DO UPDATE SET
                author = excluded.author,
                channel_id = excluded.channel_id,
                response_id = excluded.response_id,
                requested_at = excluded.requested_at,
                job_type = excluded.job_type,
                preset_type = excluded.preset_type,
                preset_name = excluded.preset_name,
                candidates = excluded.candidates,
                outro = excluded.outro,
                link = excluded.link,
                directory = excluded.directory,
                stage = excluded.stage,
                server_id = excluded.server_id,
                episode = excluded.episode,
                worker = excluded.worker,
                started_at = NULL,
                ended_at = NULL,
                cancel_reason = NULL,
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
        .bind(&preset_name)
        .bind(candidates)
        .bind(outro)
        .bind(link)
        .bind(job.directory.to_string_lossy().to_string())
        .bind(stage_to_int(job.ready))
        .bind(job.server_id.map(|id| id as i64))
        .bind(episode)
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
        let stage_value = stage_to_int(stage);
        let now = unix_secs();
        let terminal = is_terminal_stage(stage);
        let result = sqlx::query(
            r#"
            UPDATE jobs SET
                stage = ?,
                started_at = CASE
                    WHEN started_at IS NULL AND ? NOT IN (0) THEN ?
                    ELSE started_at
                END,
                ended_at = CASE WHEN ? THEN COALESCE(ended_at, ?) ELSE ended_at END
            WHERE job_id = ? AND stage != ?
            "#,
        )
            .bind(stage_value)
            .bind(stage_value)
            .bind(now as i64)
            .bind(terminal)
            .bind(now as i64)
            .bind(job_id as i64)
            .bind(stage_value)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() > 0 && (terminal || stage == Stage::Probed) {
            crate::pnworker::snapshot::record_job_event(job_id, stage);
        }
        Ok(())
    }

    pub async fn set_cancel_reason(&self, job_id: u64, reason: Option<&str>) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET cancel_reason = ? WHERE job_id = ?")
            .bind(reason)
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
            "UPDATE jobs SET stage = 7, ended_at = COALESCE(ended_at, ?) WHERE archived = 0 AND stage NOT IN (6, 7, 8, 9)"
        )
        .bind(unix_secs() as i64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn get_job(&self, job_id: u64) -> Result<Option<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!("WHERE job_id = ?"))
            .bind(job_id as i64)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn get_active_jobs(&self) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!("WHERE archived = 0 ORDER BY requested_at ASC"))
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_ongoing_jobs(&self) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!(
            "WHERE archived = 0 AND stage NOT IN (6, 7, 8, 9) ORDER BY requested_at ASC"
        ))
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_recent_jobs(&self, limit: i64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!("ORDER BY requested_at DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_recent_jobs_since(&self, limit: i64, since: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!("WHERE requested_at >= ? ORDER BY requested_at DESC LIMIT ?"))
            .bind(since as i64)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
    }

    // One query for a batch's children instead of one per episode. The batch output page polls every
    // five seconds and a two-cour batch is fifty sequential round-trips through a five-connection
    // pool, so the whole page waited on latency it never needed to pay. Chunked because SQLite binds
    // each id as its own parameter and older builds cap a statement at 999 of them; an empty slice
    // returns nothing rather than building the `IN ()` that is a syntax error.
    pub async fn get_jobs_by_ids(&self, job_ids: &[u64]) -> Result<Vec<JobRow>, sqlx::Error> {
        let mut rows = Vec::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(500) {
            let placeholders = ["?"].repeat(chunk.len()).join(",");
            let sql = format!("{}WHERE job_id IN ({})", job_query!(""), placeholders);
            let mut query = sqlx::query_as::<_, JobRow>(&sql);
            for job_id in chunk {
                query = query.bind(*job_id as i64);
            }
            rows.extend(query.fetch_all(&self.pool).await?);
        }
        Ok(rows)
    }

    // Stamped when the API accepts a submit, before the worker has written the row. The identity is
    // `acct:<username>` for a signed-in account and `token:<md5>` for a bare token, so the table
    // never stores a live credential and an account keeps its jobs across a token rotation.
    pub async fn set_job_owner(&self, job_id: u64, identity: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO job_owners (job_id, identity, owned_at) VALUES (?, ?, ?)
             ON CONFLICT(job_id) DO UPDATE SET identity = excluded.identity, owned_at = excluded.owned_at",
        )
        .bind(job_id as i64)
        .bind(identity)
        .bind(unix_secs() as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn job_owner(&self, job_id: u64) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query("SELECT identity FROM job_owners WHERE job_id = ?")
            .bind(job_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row.try_get::<String, _>("identity")).transpose()
    }

    // The owners of one page of jobs. Chunked for the same reason `get_jobs_by_ids` is: SQLite
    // binds every id as its own parameter and older builds cap a statement at 999 of them.
    pub async fn job_owners(
        &self,
        job_ids: &[u64],
    ) -> Result<std::collections::HashMap<i64, String>, sqlx::Error> {
        let mut owners = std::collections::HashMap::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(500) {
            let placeholders = ["?"].repeat(chunk.len()).join(",");
            let sql = format!(
                "SELECT job_id, identity FROM job_owners WHERE job_id IN ({})",
                placeholders
            );
            let mut query = sqlx::query(&sql);
            for job_id in chunk {
                query = query.bind(*job_id as i64);
            }
            for row in query.fetch_all(&self.pool).await? {
                owners.insert(row.try_get::<i64, _>("job_id")?, row.try_get::<String, _>("identity")?);
            }
        }
        Ok(owners)
    }

    // What one channel has finished uploading, newest first, for `/smartlist`. Ordered by
    // `requested_at` rather than `ended_at` so a re-encode queued after the one it replaces wins
    // even when it finished first, and archived rows are kept: a job's links outlive its work
    // directory, and every episode uploaded more than a few days ago is archived.
    pub async fn get_uploaded_jobs_by_channel(&self, channel_id: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!(
            "WHERE channel_id = ? AND stage = 6 AND uploaded_links IS NOT NULL ORDER BY requested_at DESC"
        ))
        .bind(channel_id as i64)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_jobs_by_author(&self, author: u64) -> Result<Vec<JobRow>, sqlx::Error> {
        sqlx::query_as::<_, JobRow>(job_query!("WHERE author = ? ORDER BY requested_at DESC"))
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
    pub started_at:   Option<i64>,
    pub ended_at:     Option<i64>,
    pub cancel_reason: Option<String>,
    pub job_type:     i64,
    pub preset_type:  i64,
    pub preset_name:  Option<String>,
    pub candidates:   Option<String>,
    pub outro:        Option<String>,
    pub link:         String,
    pub directory:    String,
    pub stage:        i64,
    pub archived:     i64,
    pub progress:        Option<String>,
    pub uploaded_links:  Option<String>,
    pub acix_pending:    Option<String>,
    pub server_id:       Option<i64>,
    pub episode:         Option<i64>,
    pub worker:          String,
}

// The hosts a finished job can have been uploaded to, in the order they are shown. The stored
// object also holds private Drive metadata and encode warnings, so the keys are named rather than
// iterated over.
pub const UPLOADED_LINK_KEYS: &[&str] = &["drive", "byse", "lulustream", "voe", "hls"];

impl JobRow {
    // Every host link the job recorded. A link the Drive cleanup has since redacted reads back as
    // null and drops out here, which is the point: it no longer resolves.
    pub fn uploaded_link_urls(&self) -> Vec<String> {
        let Some(value) = self
            .uploaded_links
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        else {
            return Vec::new();
        };
        let mut links: Vec<String> = Vec::new();
        for key in UPLOADED_LINK_KEYS {
            let Some(link) = value.get(*key).and_then(|item| item.as_str()) else {
                continue;
            };
            let link = link.trim();
            if (link.starts_with("https://") || link.starts_with("http://"))
                && !links.iter().any(|existing| existing == link)
            {
                links.push(link.to_string());
            }
        }
        links
    }

    // The episode this job encoded. The column is written when the job is queued, so it is empty on
    // every row inserted before the column existed and on a job whose episode only became known
    // when AnimeciX queued its publish record at upload time; both still answer from that record.
    pub fn episode_number(&self) -> Option<i64> {
        if let Some(episode) = self.episode.filter(|episode| *episode >= 1) {
            return Some(episode);
        }
        self.acix_pending
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| value.pointer("/acix/episode_num").and_then(|num| num.as_i64()))
            .filter(|episode| *episode >= 1)
    }

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
    pub requested_at: u64,
    pub started_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub cancel_reason: Option<String>,
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
            requested_at: row.requested_at.max(0) as u64,
            started_at: row.started_at.map(|value| value.max(0) as u64),
            ended_at: row.ended_at.map(|value| value.max(0) as u64),
            cancel_reason: row.cancel_reason.clone(),
            job_type:   job_type_label(row.job_type).to_string(),
            preset:     row.preset_display(),
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
        16 => "Batch",
        17 => "Subs",
        _ => "Unknown",
    }
}

impl JobRow {
    // What to show for this row's preset. Every compiled-in preset has a label of its own; a preset
    // that only ever existed as a file is named by the name it was selected under, which is the
    // only thing that identifies it.
    pub fn preset_display(&self) -> String {
        match preset_label(self.preset_type) {
            "Unknown" => self
                .preset_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Unknown".to_string()),
            label => label.to_string(),
        }
    }
}

pub fn preset_label(preset_type: i64) -> &'static str {
    match preset_type {
        0 => "PseudoLossless",
        1 => "Standard",
        2 => "Gpu",
        3 => "Dummy",
        4 => "Copy",
        5 => "VerySlow",
        6 => "720p",
        7 => "480p",
        8 => "Av1",
        _ => "Unknown",
    }
}

fn concat_to_db(folder: &Option<String>) -> Option<String> {
    folder.clone()
}

pub fn stage_to_int(stage: Stage) -> i64 {
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

fn is_terminal_stage(stage: Stage) -> bool {
    matches!(stage, Stage::Uploaded | Stage::Failed | Stage::Declined | Stage::Cancelled)
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(uploaded_links: Option<&str>, acix_pending: Option<&str>, episode: Option<i64>) -> JobRow {
        JobRow {
            job_id: 1,
            author: 2,
            channel_id: 3,
            response_id: 4,
            requested_at: 5,
            started_at: None,
            ended_at: None,
            cancel_reason: None,
            job_type: 1,
            preset_type: 1,
            preset_name: None,
            candidates: None,
            outro: None,
            link: String::new(),
            directory: String::new(),
            stage: 6,
            archived: 0,
            progress: None,
            uploaded_links: uploaded_links.map(str::to_string),
            acix_pending: acix_pending.map(str::to_string),
            server_id: None,
            episode,
            worker: "que-main".to_string(),
        }
    }

    #[test]
    fn every_host_is_listed_once_in_display_order() {
        let stored = r#"{"drive":"https://drive.google.com/file/d/abc/view","byse":"https://byse.sx/e/xyz",
            "lulustream":null,"voe":"https://voe.sx/e/qrs","hls":"https://lumiere.example/master.m3u8",
            "drive_file_id":"abc","warnings":["something"]}"#;
        assert_eq!(
            row(Some(stored), None, None).uploaded_link_urls(),
            vec![
                "https://drive.google.com/file/d/abc/view",
                "https://byse.sx/e/xyz",
                "https://voe.sx/e/qrs",
                "https://lumiere.example/master.m3u8",
            ]
        );
    }

    #[test]
    fn a_job_with_no_usable_links_lists_nothing() {
        // Backup rows store `{"drive": null}` once the cleanup has redacted the upload, and a
        // BackupAll row stores episode text rather than links at all.
        assert!(row(Some(r#"{"drive":null}"#), None, None).uploaded_link_urls().is_empty());
        assert!(row(Some(r#"{"episodes":"01 done"}"#), None, None).uploaded_link_urls().is_empty());
        assert!(row(Some("not json"), None, None).uploaded_link_urls().is_empty());
        assert!(row(None, None, None).uploaded_link_urls().is_empty());
    }

    #[test]
    fn the_column_names_the_episode_and_the_publish_record_is_the_fallback() {
        let pending = r#"{"status":"pending","acix":{"name":"Anime","mal_id":20,"season_num":1,
            "episode_num":7,"template":50,"extra":""},"drive":"https://drive.example/video"}"#;
        assert_eq!(row(None, None, Some(3)).episode_number(), Some(3));
        assert_eq!(row(None, Some(pending), Some(3)).episode_number(), Some(3));
        // Written before the column existed, or queued by a path that only learned the episode
        // when AnimeciX recorded it at upload time.
        assert_eq!(row(None, Some(pending), None).episode_number(), Some(7));
        assert_eq!(row(None, None, None).episode_number(), None);
    }

    #[test]
    fn a_movie_records_no_episode_and_is_not_invented_one() {
        // `/smartcode` on a Movie channel queues `episode_num: null`, and a zero would sort ahead
        // of every real episode if it were ever stored.
        let movie = r#"{"status":"pending","acix":{"name":"Film","mal_id":20,"season_num":null,
            "episode_num":null,"template":50,"extra":""},"drive":"https://drive.example/video"}"#;
        assert_eq!(row(None, Some(movie), None).episode_number(), None);
        assert_eq!(row(None, None, Some(0)).episode_number(), None);
    }
}
