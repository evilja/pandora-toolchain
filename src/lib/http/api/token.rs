use axum::{
    Json,
    extract::Extension,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::lib::env::standard::API_TOKENS_PATH;

use super::core::{ApiAuth, presented_token};

// Self-revocation: the caller destroys the token it is presenting, and nothing else. A token handed
// out for one piece of work should not need `/gentoken` access or a hand-edit of api.pandora to
// take back, and any token may do this — being able to prove you hold a token is the only
// authority needed to throw it away.
pub(super) async fn revoke(Extension(auth): Extension<ApiAuth>) -> Response {
    let Some(token) = presented_token(&auth) else {
        // A session is a person's sign-in, not a line in `api.pandora`. Ending it is
        // `POST /account/logout`; saying so is more useful than a bare 401.
        return (
            StatusCode::BAD_REQUEST,
            "this request is authorised by an account session, not a token — sign out instead",
        )
            .into_response();
    };
    let contents = match tokio::fs::read_to_string(API_TOKENS_PATH).await {
        Ok(contents) => contents,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read the token file: {}", e),
            )
                .into_response();
        }
    };
    let (remaining, removed) = remove_token(&contents, &token);
    if removed == 0 {
        // The middleware matched it a moment ago, so this means the file changed underneath us.
        return (StatusCode::NOT_FOUND, "token is no longer in the token file").into_response();
    }
    if let Err(e) = tokio::fs::write(API_TOKENS_PATH, remaining).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not write the token file: {}", e),
        )
            .into_response();
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "revoked": true, "lines_removed": removed })),
    )
        .into_response()
}

// Drops the token's line and the `;` label comment immediately above it, since `parse_token_file`
// reads that comment as the token's label and leaving it would relabel whichever token comes next.
fn remove_token(contents: &str, token: &str) -> (String, usize) {
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0usize;
    for line in contents.lines() {
        let stored = line.trim().split('|').next().unwrap_or("").trim();
        if !stored.is_empty() && stored == token {
            // The label belongs to this token; take it with the line it describes.
            if kept
                .last()
                .map(|previous| previous.trim().starts_with(';'))
                .unwrap_or(false)
            {
                kept.pop();
            }
            removed += 1;
            continue;
        }
        kept.push(line);
    }
    let mut out = kept.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoking_takes_the_label_line_with_the_token() {
        let file = "; PNwitch (added 1)\nAAA\n; other (added 2)\nBBB|local|42\n";
        let (out, removed) = remove_token(file, "AAA");
        assert_eq!(removed, 1);
        assert_eq!(out, "; other (added 2)\nBBB|local|42\n");
    }

    #[test]
    fn a_local_token_is_matched_by_its_token_part_only() {
        let file = "AAA\nBBB|local|42\n";
        let (out, removed) = remove_token(file, "BBB");
        assert_eq!(removed, 1);
        assert_eq!(out, "AAA\n");
    }

    #[test]
    fn every_copy_of_the_token_goes_and_unrelated_lines_stay() {
        let file = "; keep me (added 1)\nKEEP\nAAA\nAAA\n";
        let (out, removed) = remove_token(file, "AAA");
        assert_eq!(removed, 2);
        assert_eq!(out, "; keep me (added 1)\nKEEP\n");
    }

    #[test]
    fn an_unknown_token_changes_nothing() {
        let file = "; a (added 1)\nAAA\n";
        let (out, removed) = remove_token(file, "ZZZ");
        assert_eq!(removed, 0);
        assert_eq!(out, "; a (added 1)\nAAA\n");
    }
}
