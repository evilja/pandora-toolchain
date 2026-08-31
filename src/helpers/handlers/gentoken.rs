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

    // The purpose is a field on the token line rather than a note in the label: it decides which
    // presets ever reach the node, and a scheduling rule read out of free text is a rule one typo
    // away from sending a GPU encode to a machine that has no GPU.
    let node_purpose = purpose
        .as_deref()
        .map(NodePurpose::parse)
        .unwrap_or_default();
    let line = match (local_server_id, link_node.as_deref()) {
        (Some(id), _) => format!("{}|local|{}", token, id),
        (None, Some(node)) => format!("{}|link|{}|{}", token, node, node_purpose.label()),
        (None, None) => token.clone(),
    };

    if let Err(e) = append_token_line(&label, &line).await {
        command_error(ctx, command, e).await;
        return;
    }

    let labelled = label.map(|l| format!(" for `{}`", l)).unwrap_or_default();
    let scope = match (local_server_id, link_node.as_deref()) {
        (Some(id), _) => format!(" It's bound to this server (`{}`): it prefers this server's Lumiere Drive profile when configured, unlocks the git console (`/init`, `/attach`, `/source`) at `/git`, and sees this server's jobs on the console.", id),
        (None, Some(node)) => format!(" It's a Pandora Mini node token for `{}`, marked `{}`: it opens the link routes and nothing else — it cannot submit jobs, read logs, or reach git, and it is only ever offered presets its mark allows. Set it as `link_node_token` on that node, with `link_node_name|pntools|{}`.", node, node_purpose.label(), node),
        (None, None) => " It sees the jobs it submits itself. For a server's whole queue use `local:true`; for the entire deployment use `/genwitchtoken`.".to_string(),
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

// The Witch-tier counterpart of `/gentoken`. Privilege used to be granted by labelling a token
// `PNwitch` and hoping nobody renamed it; it is now a field on the token line, and this is what
// writes it. Minting is the only way to set it, so a privilege cannot be typed into existence by
// editing a comment.
pub async fn handle_genwitchtoken(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let label = option_trimmed(command, "label");
    let local = option_bool(command, "local").unwrap_or(false);
    if let Some(l) = &label {
        if l.contains('\n') || l.contains('\r') {
            command_error(ctx, command, "Error: `label` cannot contain newlines.").await;
            return;
        }
    }
    let local_server_id = if local {
        match command_server_id(ctx, command, "/genwitchtoken local").await {
            Some(id) => Some(id),
            None => return,
        }
    } else {
        None
    };

    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            command_error(ctx, command, format!("Failed to generate token: {}", e)).await;
            return;
        }
    };

    // `|witch` composes with the kind fields rather than replacing them, so a privileged token can
    // still be the server-bound one the git and Studio routes want.
    let line = match local_server_id {
        Some(id) => format!("{}|local|{}|witch", token, id),
        None => format!("{}|witch", token),
    };
    if let Err(e) = append_token_line(&label, &line).await {
        command_error(ctx, command, e).await;
        return;
    }

    let labelled = label.map(|l| format!(" for `{}`", l)).unwrap_or_default();
    let bound = match local_server_id {
        Some(id) => format!(" It is also bound to this server (`{}`), so the git and Studio routes accept it.", id),
        None => String::new(),
    };
    let embed = success_embed(command, COMMAND_UPDATED)
        .description(format!(
            "Created a **privileged** API bearer token{}.{} It sees every job in the deployment, opens `/workers`, the job logs, `/gitsync` and the Users page at `/users`, and can enrol a privileged console account. It is stored in `{}` and shown only here.",
            labelled, bound, TOKENS_PATH
        ))
        .field("Bearer token", format!("```\n{}\n```", token), false)
        .field("Usage", "`Authorization: Bearer <token>`", false)
        .field(
            "Accounts",
            "Sign in at `/login` and create an account from this token — the account inherits its privilege, and taking that privilege away later is a switch on the Users page rather than a token to hunt down.",
            false,
        );
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .ephemeral(true)
    )).await.ok();
}

// Appends one token line, with its `;` label comment above it. Shared by both minting commands so
// the label convention `parse_token_file` reads has exactly one writer.
async fn append_token_line(label: &Option<String>, line: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(TOKENS_PATH).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create token dir: {}", e))?;
    }
    let mut blob = String::from("\n");
    if let Some(l) = label {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        blob.push_str(&format!("; {} (added {})\n", l, ts));
    }
    blob.push_str(line);
    blob.push('\n');
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(TOKENS_PATH)
        .await
        .map_err(|e| format!("Failed to write token: {}", e))?;
    f.write_all(blob.as_bytes())
        .await
        .map_err(|e| format!("Failed to write token: {}", e))
}

fn generate_token() -> Result<String, String> {
    pandora_toolchain::lib::secret::random_hex_token()
        .map_err(|e| format!("entropy source failed: {}", e))
}
