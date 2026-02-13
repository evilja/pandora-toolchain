# libpndb Documentation

## Overview

`libpndb` is a PostgreSQL database library for the Pandora Toolchain that provides persistent storage for encoding jobs. It stores all job information as a permanent log, allowing you to track job history, monitor active jobs, and analyze past operations.

## Features

- **Persistent Job Storage**: All jobs are stored in PostgreSQL and never deleted
- **Job Lifecycle Tracking**: Track jobs from queued to uploaded/failed states
- **Job Archiving**: Completed jobs are marked as archived but remain in the database
- **Query Capabilities**: Search by user, stage, or view entire job history
- **Automatic Cleanup**: No manual connection management needed

## Installation

Add to your `Cargo.toml`:
```toml
[dependencies]
tokio-postgres = "0.7"
```

Add to `src/lib.rs`:
```rust
pub mod libpndb;
```

## Quick Start

```rust
use pandora_toolchain::libpndb::core::JobDb;

// Connect to database
let db = JobDb::new("host=localhost user=postgres password=secret dbname=pandora").await?;

// Create tables (run once on first startup)
db.init_schema().await?;

// Insert a new job
db.insert_job(&job).await?;

// Update job progress
db.update_stage(job_id, Stage::Downloaded).await?;

// Archive completed job (keeps it in database)
db.archive_job(job_id).await?;
```

## Database Schema

The library creates a single `jobs` table:

| Column | Type | Description |
|--------|------|-------------|
| `job_id` | BIGINT | Primary key, unique job identifier |
| `author` | TEXT | Discord username who requested the job |
| `requested_at` | BIGINT | Unix timestamp when job was created |
| `job_type` | SMALLINT | Type of job (currently only Encode=1) |
| `preset` | SMALLINT | Encoding preset (0-3) |
| `preset_concat` | SMALLINT | Concat subtype if preset=3 |
| `link` | TEXT | Download link for the source file |
| `directory` | TEXT | Full path to job working directory |
| `ready` | SMALLINT | Current job stage (0-7) |
| `message_id` | BIGINT | Discord message ID for updates |
| `channel_id` | BIGINT | Discord channel ID |
| `created_at` | TIMESTAMP | When the row was created |
| `completed_at` | TIMESTAMP | When the job was archived |
| `archived` | BOOLEAN | Whether job is completed/archived |

### Indexes

- `idx_jobs_ready`: Fast queries for jobs by stage (only non-archived)
- `idx_jobs_author`: Fast queries for user's job history
- `idx_jobs_archived`: Fast queries for archived vs active jobs

## API Reference

### Creating a Connection

#### `JobDb::new(connection_string: &str) -> Result<Self, Error>`

Connects to PostgreSQL and returns a `JobDb` instance.

**Parameters:**
- `connection_string`: PostgreSQL connection string (e.g., `"host=localhost user=postgres password=secret dbname=pandora"`)

**Returns:**
- `Result<JobDb, Error>`: Database handle or connection error

**Example:**
```rust
let db = JobDb::new("host=localhost user=postgres dbname=pandora").await?;
```

**Note:** No need to manually close the connection - it's handled automatically when `db` goes out of scope.

---

### Schema Management

#### `init_schema() -> Result<(), Error>`

Creates the `jobs` table and all indexes if they don't exist. Safe to call multiple times.

**Example:**
```rust
db.init_schema().await?;
```

**Best Practice:** Run this once during application startup.

---

### Inserting Jobs

#### `insert_job(job: &Job) -> Result<(), Error>`

Inserts a new job into the database. Automatically encodes the preset and stage information.

**Parameters:**
- `job`: Reference to a `Job` struct from `pnworker::core`

**Example:**
```rust
let job = Job::new(
    "username".to_string(),
    JobType::Encode,
    12345,
    Preset::Standard,
    "https://example.com/video.mkv".to_string(),
    (context, message)
);

db.insert_job(&job).await?;
```

**Note:** This stores all job metadata except the Serenity `Context` (which can't be serialized). Message and channel IDs are extracted and stored for updates.

---

### Updating Jobs

#### `update_stage(job_id: u64, stage: Stage) -> Result<(), Error>`

Updates a job's current stage/status.

**Parameters:**
- `job_id`: The unique job identifier
- `stage`: New stage (Queued, Downloading, Downloaded, Encoding, Encoded, Uploading, Uploaded, Failed)

**Example:**
```rust
db.update_stage(12345, Stage::Downloaded).await?;
db.update_stage(12345, Stage::Encoding).await?;
db.update_stage(12345, Stage::Uploaded).await?;
```

**Common Flow:**
```
Queued → Downloading → Downloaded → Encoding → Encoded → Uploading → Uploaded
                                                                    → Failed
```

---

### Archiving Jobs

#### `archive_job(job_id: u64) -> Result<(), Error>`

Marks a job as archived and records the completion time. **The job is NOT deleted** - it remains in the database for historical purposes.

**Parameters:**
- `job_id`: The unique job identifier

**Example:**
```rust
// When job completes successfully
db.update_stage(job_id, Stage::Uploaded).await?;
db.archive_job(job_id).await?;

// Or when job fails
db.update_stage(job_id, Stage::Failed).await?;
db.archive_job(job_id).await?;
```

**Note:** Archived jobs are excluded from "active jobs" queries but can still be retrieved via history queries.

---

### Querying Jobs

#### `get_job(job_id: u64) -> Result<Option<JobRow>, Error>`

Retrieves a single job by ID, whether active or archived.

**Returns:**
- `Some(JobRow)` if found
- `None` if not found

**Example:**
```rust
if let Some(job_row) = db.get_job(12345).await? {
    println!("Job by {} at stage {:?}", job_row.author, job_row.ready);
}
```

---

#### `get_jobs_by_stage(stage: Stage) -> Result<Vec<JobRow>, Error>`

Gets all **active** (non-archived) jobs in a specific stage, ordered by request time.

**Parameters:**
- `stage`: The stage to filter by

**Example:**
```rust
// Get all jobs currently downloading
let downloading = db.get_jobs_by_stage(Stage::Downloading).await?;

// Get all jobs waiting to be encoded
let ready_to_encode = db.get_jobs_by_stage(Stage::Downloaded).await?;
```

**Use Case:** Processing pipeline - find jobs ready for the next step.

---

#### `get_active_jobs() -> Result<Vec<JobRow>, Error>`

Gets all **active** (non-archived) jobs, regardless of stage. Ordered by request time (oldest first).

**Example:**
```rust
let active = db.get_active_jobs().await?;
println!("Active jobs: {}", active.len());

for job in active {
    println!("Job {} by {} - Stage: {}", job.job_id, job.author, job.ready);
}
```

**Use Case:** Queue management, dashboard displays.

---

#### `get_queue_length() -> Result<i64, Error>`

Returns the count of active (non-archived) jobs.

**Example:**
```rust
let count = db.get_queue_length().await?;
if count > 10 {
    println!("Queue is full!");
}
```

**Use Case:** Queue limits, capacity checks before accepting new jobs.

---

#### `get_user_history(author: &str, limit: i64) -> Result<Vec<JobRow>, Error>`

Gets a user's job history (both active and archived), most recent first.

**Parameters:**
- `author`: Discord username
- `limit`: Maximum number of jobs to return

**Example:**
```rust
// Get user's last 10 jobs
let history = db.get_user_history("username", 10).await?;

for job in history {
    let status = if job.archived { "Completed" } else { "Active" };
    println!("Job {}: {} - Stage: {}", job.job_id, status, job.ready);
}
```

**Use Case:** User commands like `!myjobs`, rate limiting per user.

---

#### `get_completed_jobs(limit: i64) -> Result<Vec<JobRow>, Error>`

Gets recently completed (archived) jobs, most recent first.

**Parameters:**
- `limit`: Maximum number of jobs to return

**Example:**
```rust
// Get last 50 completed jobs
let completed = db.get_completed_jobs(50).await?;

for job in completed {
    println!("Completed: {} by {}", job.job_id, job.author);
}
```

**Use Case:** Analytics, completion logs, debugging recent issues.

---

## Data Types

### JobRow

Raw database row representation. Contains numeric IDs that need decoding.

```rust
pub struct JobRow {
    pub job_id: u64,
    pub author: String,
    pub requested_at: u64,        // Unix timestamp in seconds
    pub job_type: i16,             // Encoded JobType
    pub preset: i16,               // Encoded Preset
    pub preset_concat: Option<i16>, // Concat subtype if applicable
    pub link: String,
    pub directory: String,
    pub ready: i16,                // Encoded Stage
    pub message_id: u64,
    pub channel_id: u64,
    pub archived: bool,
}
```

**Convert to friendly format:**
```rust
let metadata = job_row.to_job_metadata();
```

---

### JobMetadata

Human-readable job information with decoded enums.

```rust
pub struct JobMetadata {
    pub job_id: u64,
    pub author: String,
    pub requested_at: Duration,
    pub job_type: JobType,          // Decoded enum
    pub preset: Preset,             // Decoded enum
    pub link: String,
    pub directory: PathBuf,
    pub ready: Stage,               // Decoded enum
    pub message_id: u64,
    pub channel_id: u64,
    pub archived: bool,
}
```

**Example:**
```rust
let job_row = db.get_job(12345).await?.unwrap();
let metadata = job_row.to_job_metadata();

match metadata.preset {
    Preset::Standard => println!("Using standard preset"),
    Preset::Gpu => println!("Using GPU acceleration"),
    _ => {}
}
```

---

## Encoding Reference

The library automatically converts Rust enums to database integers:

### JobType
| Enum | Database Value |
|------|---------------|
| `JobType::Encode` | 1 |

### Preset
| Enum | preset | preset_concat |
|------|--------|---------------|
| `Preset::PseudoLossless` | 0 | NULL |
| `Preset::Standard` | 1 | NULL |
| `Preset::Gpu` | 2 | NULL |
| `Preset::Concat(Concat::Default)` | 3 | 0 |
| `Preset::Concat(Concat::SomeSubs)` | 3 | 1 |

### Stage
| Enum | Database Value |
|------|---------------|
| `Stage::Queued` | 0 |
| `Stage::Downloading` | 1 |
| `Stage::Downloaded` | 2 |
| `Stage::Encoding` | 3 |
| `Stage::Encoded` | 4 |
| `Stage::Uploading` | 5 |
| `Stage::Uploaded` | 6 |
| `Stage::Failed` | 7 |

---

## Integration Example

Here's how to integrate with the existing `pnworker`:

```rust
// In your main application startup
let db = JobDb::new("host=localhost user=postgres dbname=pandora").await?;
db.init_schema().await?;

// When receiving a new job
let job = Job::new(/* ... */);
db.insert_job(&job).await?;

// In your worker loops
pub async fn pn_dloadworker(mut rx: Receiver<DownloadData>, tx: Sender<CommData>, db: JobDb) {
    loop {
        if let Some((directory, link, job_id)) = rx.recv().await {
            // Do download work...
            
            // Update database
            db.update_stage(job_id, Stage::Downloaded).await.ok();
            
            // Notify main worker
            tx.send((job_id, "Downloaded".to_string(), Some(Stage::Downloaded))).await.ok();
        }
    }
}

// When job completes
db.update_stage(job_id, Stage::Uploaded).await?;
db.archive_job(job_id).await?;

// When job fails
db.update_stage(job_id, Stage::Failed).await?;
db.archive_job(job_id).await?;
```

---

## Common Patterns

### Check Queue Before Accepting Job
```rust
let queue_length = db.get_queue_length().await?;
if queue_length >= 5 {
    msg.reply(&ctx, "Queue is full, try again later").await?;
    return;
}
```

### Show User Their Job History
```rust
let history = db.get_user_history(&msg.author.name, 10).await?;
let mut response = format!("Your last {} jobs:\n", history.len());
for job in history {
    let status = if job.archived { "✓" } else { "⏳" };
    response.push_str(&format!("{} Job {} - Stage: {}\n", status, job.job_id, job.ready));
}
msg.reply(&ctx, response).await?;
```

### Restart After Crash
```rust
// On startup, recover any jobs that were interrupted
let active = db.get_active_jobs().await?;
for job_row in active {
    let metadata = job_row.to_job_metadata();
    // Re-queue or mark as failed based on stage
    match metadata.ready {
        Stage::Downloading | Stage::Encoding | Stage::Uploading => {
            db.update_stage(metadata.job_id, Stage::Failed).await?;
            db.archive_job(metadata.job_id).await?;
        }
        _ => {}
    }
}
```

---

## Best Practices

1. **Always call `init_schema()` on startup** - It's safe to call multiple times
2. **Archive jobs when complete** - Use `archive_job()` instead of deleting
3. **Update stages atomically** - Call `update_stage()` immediately after state changes
4. **Use transactions for critical operations** - Consider wrapping multi-step operations in transactions
5. **Don't store Context** - The library correctly extracts message/channel IDs instead
6. **Query by stage for pipeline processing** - Use `get_jobs_by_stage()` to find work
7. **Monitor queue length** - Check before accepting new jobs to prevent overload

---

## Error Handling

All database methods return `Result<T, tokio_postgres::Error>`. Common errors:

- **Connection failed**: Check connection string and PostgreSQL is running
- **Table doesn't exist**: Call `init_schema()` first
- **Constraint violation**: Trying to insert duplicate job_id

**Example:**
```rust
match db.insert_job(&job).await {
    Ok(_) => println!("Job inserted"),
    Err(e) => eprintln!("Database error: {}", e),
}
```

---

## Performance Considerations

- **Indexes are created automatically** for common queries
- **Archived jobs don't slow down active queries** due to filtered indexes
- **Connection pooling**: For high-throughput apps, consider using `deadpool-postgres`
- **Batch operations**: If inserting many jobs, consider using transactions

---

## Maintenance

### View All Jobs
```sql
SELECT job_id, author, ready, archived FROM jobs ORDER BY requested_at DESC LIMIT 100;
```

### Cleanup Old Archived Jobs (if needed)
```sql
DELETE FROM jobs WHERE archived = TRUE AND completed_at < NOW() - INTERVAL '90 days';
```

### See Queue Status
```sql
SELECT ready, COUNT(*) FROM jobs WHERE NOT archived GROUP BY ready;
```

---

## Connection String Examples

```rust
// Local development
"host=localhost user=postgres password=secret dbname=pandora"

// With port
"host=localhost port=5432 user=postgres password=secret dbname=pandora"

// Remote server
"host=db.example.com user=pandora password=secret dbname=production sslmode=require"

// Unix socket
"host=/var/run/postgresql user=postgres dbname=pandora"
```

---

## Summary

`libpndb` provides a simple, safe way to persist Pandora Toolchain jobs in PostgreSQL. Jobs are never deleted, creating a permanent audit log. The API is designed to integrate seamlessly with the existing `pnworker` architecture while adding persistence, crash recovery, and historical analysis capabilities.
