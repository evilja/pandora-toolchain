// ./src/libpndb/core.rs
use tokio_postgres::{Client, NoTls, Error};
use std::time::Duration;
use crate::pnworker::core::{Job, JobType, Preset, Concat, Stage};
use std::path::PathBuf;

pub struct JobDb {
    client: Client,
}

impl JobDb {
    pub async fn new(connection_string: &str) -> Result<Self, Error> {
        let (client, connection) = tokio_postgres::connect(connection_string, NoTls).await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("connection error: {}", e);
            }
        });

        Ok(Self { client })
    }

    pub async fn init_schema(&self) -> Result<(), Error> {
        self.client.execute(
            "CREATE TABLE IF NOT EXISTS jobs (
                job_id BIGINT PRIMARY KEY,
                author TEXT NOT NULL,
                requested_at BIGINT NOT NULL,
                job_type SMALLINT NOT NULL,
                preset SMALLINT NOT NULL,
                preset_concat SMALLINT,
                link TEXT NOT NULL,
                directory TEXT NOT NULL,
                ready SMALLINT NOT NULL,
                message_id BIGINT,
                channel_id BIGINT,
                created_at TIMESTAMP DEFAULT NOW(),
                completed_at TIMESTAMP,
                archived BOOLEAN DEFAULT FALSE
            )",
            &[],
        ).await?;

        self.client.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_ready ON jobs(ready) WHERE NOT archived",
            &[],
        ).await?;

        self.client.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_author ON jobs(author)",
            &[],
        ).await?;

        self.client.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_archived ON jobs(archived)",
            &[],
        ).await?;

        Ok(())
    }

    pub async fn insert_job(&self, job: &Job) -> Result<(), Error> {
        let (preset_id, concat_id) = Self::encode_preset(&job.preset);

        self.client.execute(
            "INSERT INTO jobs
                (job_id, author, requested_at, job_type, preset, preset_concat,
                 link, directory, ready, message_id, channel_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            &[
                &(job.job_id as i64),
                &job.author,
                &(job.requested_at.as_secs() as i64),
                &(job.job_type as i16),
                &preset_id,
                &concat_id,
                &job.link,
                &job.directory.to_string_lossy().to_string(),
                &(job.ready as i16),
                &(job.context.1.id.get() as i64),
                &(job.context.1.channel_id.get() as i64),
            ],
        ).await?;
        println!("--- [JOB {} QUEUED]", job.job_id);
        Ok(())
    }

    pub async fn update_stage(&self, job_id: u64, stage: Stage) -> Result<(), Error> {
        self.client.execute(
            "UPDATE jobs SET ready = $1 WHERE job_id = $2",
            &[&(stage as i16), &(job_id as i64)],
        ).await?;
        println!("--- [JOB {} SWITCHED STAGE {:?}]", job_id, stage);
        Ok(())
    }

    pub async fn archive_job(&self, job_id: u64) -> Result<(), Error> {
        self.client.execute(
            "UPDATE jobs SET archived = TRUE, completed_at = NOW() WHERE job_id = $1",
            &[&(job_id as i64)],
        ).await?;
        println!("--- [JOB {} ARCHIVED]", job_id);
        Ok(())
    }

    pub async fn get_job(&self, job_id: u64) -> Result<Option<JobRow>, Error> {
        let row = self.client.query_opt(
            "SELECT job_id, author, requested_at, job_type, preset, preset_concat,
                    link, directory, ready, message_id, channel_id, archived
             FROM jobs WHERE job_id = $1",
            &[&(job_id as i64)],
        ).await?;

        Ok(row.map(|r| JobRow {
            job_id: r.get::<_, i64>(0) as u64,
            author: r.get(1),
            requested_at: r.get::<_, i64>(2) as u64,
            job_type: r.get::<_, i16>(3),
            preset: r.get::<_, i16>(4),
            preset_concat: r.get(5),
            link: r.get(6),
            directory: r.get(7),
            ready: r.get::<_, i16>(8),
            message_id: r.get::<_, i64>(9) as u64,
            channel_id: r.get::<_, i64>(10) as u64,
            archived: r.get(11),
        }))
    }

    pub async fn get_jobs_by_stage(&self, stage: Stage) -> Result<Vec<JobRow>, Error> {
        let rows = self.client.query(
            "SELECT job_id, author, requested_at, job_type, preset, preset_concat,
                    link, directory, ready, message_id, channel_id, archived
             FROM jobs WHERE ready = $1 AND NOT archived
             ORDER BY requested_at ASC",
            &[&(stage as i16)],
        ).await?;

        Ok(rows.iter().map(|r| JobRow {
            job_id: r.get::<_, i64>(0) as u64,
            author: r.get(1),
            requested_at: r.get::<_, i64>(2) as u64,
            job_type: r.get::<_, i16>(3),
            preset: r.get::<_, i16>(4),
            preset_concat: r.get(5),
            link: r.get(6),
            directory: r.get(7),
            ready: r.get::<_, i16>(8),
            message_id: r.get::<_, i64>(9) as u64,
            channel_id: r.get::<_, i64>(10) as u64,
            archived: r.get(11),
        }).collect())
    }

    pub async fn get_active_jobs(&self) -> Result<Vec<JobRow>, Error> {
        let rows = self.client.query(
            "SELECT job_id, author, requested_at, job_type, preset, preset_concat,
                    link, directory, ready, message_id, channel_id, archived
             FROM jobs
             WHERE ready < $1 AND NOT archived
             ORDER BY requested_at ASC",
            &[&(Stage::Uploaded as i16)],
        ).await?;

        Ok(rows.iter().map(|r| JobRow {
            job_id: r.get::<_, i64>(0) as u64,
            author: r.get(1),
            requested_at: r.get::<_, i64>(2) as u64,
            job_type: r.get::<_, i16>(3),
            preset: r.get::<_, i16>(4),
            preset_concat: r.get(5),
            link: r.get(6),
            directory: r.get(7),
            ready: r.get::<_, i16>(8),
            message_id: r.get::<_, i64>(9) as u64,
            channel_id: r.get::<_, i64>(10) as u64,
            archived: r.get(11),
        }).collect())
    }

    pub async fn get_queue_length(&self) -> Result<i64, Error> {
        let row = self.client.query_one(
            "SELECT COUNT(*) FROM jobs WHERE ready < $1 AND NOT archived",
            &[&(Stage::Uploaded as i16)],
        ).await?;

        Ok(row.get(0))
    }

    pub async fn get_user_history(&self, author: &str, limit: i64) -> Result<Vec<JobRow>, Error> {
        let rows = self.client.query(
            "SELECT job_id, author, requested_at, job_type, preset, preset_concat,
                    link, directory, ready, message_id, channel_id, archived
             FROM jobs
             WHERE author = $1
             ORDER BY requested_at DESC
             LIMIT $2",
            &[&author, &limit],
        ).await?;

        Ok(rows.iter().map(|r| JobRow {
            job_id: r.get::<_, i64>(0) as u64,
            author: r.get(1),
            requested_at: r.get::<_, i64>(2) as u64,
            job_type: r.get::<_, i16>(3),
            preset: r.get::<_, i16>(4),
            preset_concat: r.get(5),
            link: r.get(6),
            directory: r.get(7),
            ready: r.get::<_, i16>(8),
            message_id: r.get::<_, i64>(9) as u64,
            channel_id: r.get::<_, i64>(10) as u64,
            archived: r.get(11),
        }).collect())
    }

    pub async fn get_completed_jobs(&self, limit: i64) -> Result<Vec<JobRow>, Error> {
        let rows = self.client.query(
            "SELECT job_id, author, requested_at, job_type, preset, preset_concat,
                    link, directory, ready, message_id, channel_id, archived
             FROM jobs
             WHERE archived = TRUE
             ORDER BY completed_at DESC
             LIMIT $1",
            &[&limit],
        ).await?;

        Ok(rows.iter().map(|r| JobRow {
            job_id: r.get::<_, i64>(0) as u64,
            author: r.get(1),
            requested_at: r.get::<_, i64>(2) as u64,
            job_type: r.get::<_, i16>(3),
            preset: r.get::<_, i16>(4),
            preset_concat: r.get(5),
            link: r.get(6),
            directory: r.get(7),
            ready: r.get::<_, i16>(8),
            message_id: r.get::<_, i64>(9) as u64,
            channel_id: r.get::<_, i64>(10) as u64,
            archived: r.get(11),
        }).collect())
    }

    fn encode_preset(preset: &Preset) -> (i16, Option<i16>) {
        match preset {
            Preset::PseudoLossless(i) => (0, *i),
            Preset::Standard(i) => (1, *i),
            Preset::Gpu(i) => (2, *i),
        }
    }

    pub fn decode_preset(preset: i16, concat: Option<i16>) -> Preset {
        match (preset, concat) {
            (0, i) => Preset::PseudoLossless(i),
            (1, i) => Preset::Standard(i),
            (2, i) => Preset::Gpu(i),
            _ => Preset::Standard(Some(0)),
        }
    }

    fn decode_stage(stage: i16) -> Stage {
        match stage {
            0 => Stage::Queued,
            1 => Stage::Downloading,
            2 => Stage::Downloaded,
            3 => Stage::Encoding,
            4 => Stage::Encoded,
            5 => Stage::Uploading,
            6 => Stage::Uploaded,
            7 => Stage::Failed,
            _ => Stage::Failed,
        }
    }

    fn decode_job_type(job_type: i16) -> JobType {
        match job_type {
            1 => JobType::Encode,
            _ => JobType::Encode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub job_id: u64,
    pub author: String,
    pub requested_at: u64,
    pub job_type: i16,
    pub preset: i16,
    pub preset_concat: Option<i16>,
    pub link: String,
    pub directory: String,
    pub ready: i16,
    pub message_id: u64,
    pub channel_id: u64,
    pub archived: bool,
}

impl JobRow {
    pub fn to_job_metadata(&self) -> JobMetadata {
        JobMetadata {
            job_id: self.job_id,
            author: self.author.clone(),
            requested_at: Duration::from_secs(self.requested_at),
            job_type: JobDb::decode_job_type(self.job_type),
            preset: JobDb::decode_preset(self.preset, self.preset_concat),
            link: self.link.clone(),
            directory: PathBuf::from(&self.directory),
            ready: JobDb::decode_stage(self.ready),
            message_id: self.message_id,
            channel_id: self.channel_id,
            archived: self.archived,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobMetadata {
    pub job_id: u64,
    pub author: String,
    pub requested_at: Duration,
    pub job_type: JobType,
    pub preset: Preset,
    pub link: String,
    pub directory: PathBuf,
    pub ready: Stage,
    pub message_id: u64,
    pub channel_id: u64,
    pub archived: bool,
}
