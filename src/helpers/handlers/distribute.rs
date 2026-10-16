use super::*;

pub async fn handle_distribute(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    link: String,
) -> Option<Job> {
    let mut job = handle_backup(ctx, command, link).await?;
    job.job_type = JobType::Distribute;
    Some(job)
}
