use super::*;
use pandora_toolchain::pnworker::link::spec::NodePurpose;
use tokio::io::AsyncWriteExt;

const TOKENS_PATH: &str = pandora_toolchain::lib::env::standard::API_TOKENS_PATH;

pub async fn handle_gentoken(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let label = option_trimmed(command, "label");
    let local = option_bool(command, "local").unwrap_or(false);
    let link_node = option_trimmed(command, "link");
    let purpose = option_trimmed(command, "purpose");
    if purpose.is_some() && link_node.is_none() {
        command_error(
            ctx,
            command,
            "Error: `purpose` describes a Pandora Mini node, so it needs `link`.",
        )
        .await;
        return;
    }
    if local && link_node.is_some() {
        command_error(
            ctx,
            command,
            "Error: a token is either `local` or a `link` node token, not both.",
        )
        .await;
        return;
    }
    if let Some(node) = &link_node {
        // The node name is a field in a `|`-separated line and the identity a node authenticates
        // under, so anything that could split the line or arrive with invisible edges is refused
        // here rather than producing a token nothing can ever match.
        if node.is_empty()
            || node.contains('|')
            || node.contains('\n')
            || node.contains('\r')
            || node.chars().any(|c| c.is_whitespace())
        {
            command_error(
                ctx,
                command,
                "Error: `link` must be a node name with no spaces or `|`.",
            )
            .await;
            return;
        }
    }
    let local_server_id = if local {
        match command_server_id(ctx, command, "/gentoken local").await {
            Some(id) => Some(id),
            None => return,
        }
    } else {
        None
    };
    if let Some(l) = &label {
        if l.contains('\n') || l.contains('\r') {
            command_error(ctx, command, "Error: `label` cannot contain newlines.").await;
            return;
        }
    }

    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            command_error(ctx, command, format!("Failed to generate token: {}", e)).await;
            return;
        }
    };

    if let Some(parent) = std::path::Path::new(TOKENS_PATH).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command_error(ctx, command, format!("Failed to create token dir: {}", e)).await;
            return;
        }
    }

    let mut blob = String::from("\n");
    if let Some(l) = &label {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        blob.push_str(&format!("; {} (added {})\n", l, ts));
    }
    // The purpose is a field on the token line rather than a note in the label: it decides which
    // presets ever reach the node, and a scheduling rule read out of free text is a rule one typo
    // away from sending a GPU encode to a machine that has no GPU.
    let node_purpose = purpose
        .as_deref()
        .map(NodePurpose::parse)
        .unwrap_or_default();
    match (local_server_id, link_node.as_deref()) {
        (Some(id), _) => blob.push_str(&format!("{}|local|{}", token, id)),
        (None, Some(node)) => {
            blob.push_str(&format!("{}|link|{}|{}", token, node, node_purpose.label()))
        }
        (None, None) => blob.push_str(&token),
    }
    blob.push('\n');

    let write_result = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(TOKENS_PATH)
            .await?;
        f.write_all(blob.as_bytes()).await
    }
    .await;

    if let Err(e) = write_result {
        command_error(ctx, command, format!("Failed to write token: {}", e)).await;
        return;
    }

    let labelled = label.map(|l| format!(" for `{}`", l)).unwrap_or_default();
    let scope = match (local_server_id, link_node.as_deref()) {
        (Some(id), _) => format!(" It's bound to this server (`{}`): it prefers this server's Lumiere Drive profile when configured, and unlocks the git console (`/init`, `/attach`, `/source`) at `/git`.", id),
        (None, Some(node)) => format!(" It's a Pandora Mini node token for `{}`, marked `{}`: it opens the link routes and nothing else — it cannot submit jobs, read logs, or reach git, and it is only ever offered presets its mark allows. Set it as `link_node_token` on that node, with `link_node_name|pntools|{}`.", node, node_purpose.label(), node),
        (None, None) => String::new(),
    };
    let embed = success_embed(command, COMMAND_UPDATED)
        .description(format!(
            "Created an API bearer token{}.{} It is stored in `{}` and shown only here.",
            labelled, scope, TOKENS_PATH
        ))
        .field("Bearer token", format!("```\n{}\n```", token), false)
        .field("Usage", "`Authorization: Bearer <token>`", false);
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .ephemeral(true)
    )).await.ok();
}

fn generate_token() -> Result<String, String> {
    pandora_toolchain::lib::secret::random_hex_token()
        .map_err(|e| format!("entropy source failed: {}", e))
}
