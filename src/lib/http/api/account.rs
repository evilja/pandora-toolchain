use axum::{
    Json,
    extract::{Extension, Path},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::lib::account;

use super::core::{ApiAuth, require_privileged};

fn no_store<T: IntoResponse>(body: T) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], body).into_response()
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

// ---- unauthenticated -------------------------------------------------------

// Whether this deployment has anybody enrolled yet. The console needs it before it can decide what
// to draw on an empty browser: a sign-in form nobody can use is worse than the enrolment path. It
// answers a boolean and not a count, because how many people are here is not a stranger's business.
pub(super) async fn status() -> Response {
    no_store(Json(json!({ "has_accounts": account::any_accounts() })))
}

#[derive(Deserialize)]
pub(super) struct LoginReq {
    username: String,
    password: String,
}

// The one route that answers without a bearer credential, since it is what mints one. Its refusal
// is deliberately the same sentence for a wrong password, an unknown name, and a disabled account:
// three different answers would turn this form into a roster.
pub(super) async fn login(Json(req): Json<LoginReq>) -> Response {
    let account = match account::authenticate(&req.username, &req.password) {
        Ok(account) => account,
        Err(message) => return (StatusCode::UNAUTHORIZED, message).into_response(),
    };
    let (session, expires_at) = match account::open_session(&account.username) {
        Ok(session) => session,
        Err(message) => return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response(),
    };
    no_store(Json(json!({
        "session": session,
        "expires_at": expires_at,
        "account": account.view(),
    })))
}

// ---- the signed-in person --------------------------------------------------

#[derive(Deserialize)]
pub(super) struct RegisterReq {
    username: String,
    password: String,
}

// Enrolment: a person turning the token they were handed into an account of their own. It is
// authorised by the token, and the account inherits exactly that token's reach — a local token
// makes a server-scoped account, a privileged one makes a privileged account. Nothing here can
// widen anything, which is why it is safe for the console to offer it to whoever holds a token.
pub(super) async fn register(
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<RegisterReq>,
) -> Response {
    if auth.account.is_some() {
        return bad_request("you are already signed in as an account".to_string());
    }
    if auth.link_node.is_some() {
        return (
            StatusCode::FORBIDDEN,
            "a Pandora Mini node token is a machine's credential, not a person's",
        )
            .into_response();
    }
    let account = match account::create(
        &req.username,
        &req.password,
        auth.privileged,
        auth.local_server_id,
        auth.token_label(),
    ) {
        Ok(account) => account,
        Err(message) => return bad_request(message),
    };
    let (session, expires_at) = match account::open_session(&account.username) {
        Ok(session) => session,
        Err(message) => return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response(),
    };
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "session": session,
            "expires_at": expires_at,
            "account": account.view(),
        })),
    )
        .into_response()
}

pub(super) async fn me(Extension(auth): Extension<ApiAuth>) -> Response {
    let account = auth.account.as_deref().and_then(account::get);
    no_store(Json(json!({ "account": account.map(|account| account.view()) })))
}

pub(super) async fn logout(Extension(auth): Extension<ApiAuth>) -> Response {
    let Some(session) = auth.presented_session() else {
        return bad_request("this request is authorised by a token, not a session".to_string());
    };
    no_store(Json(json!({ "signed_out": account::close_session(&session) })))
}

#[derive(Deserialize)]
pub(super) struct PasswordReq {
    current: String,
    password: String,
}

// Changing your own password. The current one is required even though the session already proves
// who you are: a session is a browser somebody may have walked away from, and a password change
// signs every other session out.
pub(super) async fn change_password(
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<PasswordReq>,
) -> Response {
    let Some(username) = auth.account.clone() else {
        return bad_request("only a signed-in account has a password".to_string());
    };
    if account::authenticate(&username, &req.current).is_err() {
        return (StatusCode::UNAUTHORIZED, "the current password is wrong").into_response();
    }
    match account::update(
        &username,
        account::Update {
            privileged: None,
            server_id: None,
            disabled: None,
            password: Some(req.password),
        },
    ) {
        Ok(_) => no_store(Json(json!({ "changed": true }))),
        Err(message) => bad_request(message),
    }
}

// ---- the Users page --------------------------------------------------------

pub(super) async fn list(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    no_store(Json(json!({
        "accounts": account::list().iter().map(account::Account::view).collect::<Vec<_>>(),
        "you": auth.account,
    })))
}

#[derive(Deserialize)]
pub(super) struct CreateReq {
    username: String,
    password: String,
    #[serde(default)]
    privileged: bool,
    // A string, like every other snowflake this API carries: Discord ids exceed JS's safe integer
    // range, and a console that rounds one binds an account to a guild that does not exist.
    #[serde(default)]
    server_id: Option<String>,
}

pub(super) async fn create(
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<CreateReq>,
) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let server_id = match parse_server_id(req.server_id.as_deref()) {
        Ok(server_id) => server_id,
        Err(message) => return bad_request(message),
    };
    match account::create(
        &req.username,
        &req.password,
        req.privileged,
        server_id,
        auth.account.clone().map(|who| format!("created by {}", who)),
    ) {
        Ok(account) => (
            StatusCode::CREATED,
            [(header::CACHE_CONTROL, "no-store")],
            Json(account.view()),
        )
            .into_response(),
        Err(message) => bad_request(message),
    }
}

#[derive(Deserialize)]
pub(super) struct UpdateReq {
    #[serde(default)]
    privileged: Option<bool>,
    // Two levels of absence, and they mean different things: the field left out changes nothing,
    // the field sent as null unbinds the account from its server.
    #[serde(default, deserialize_with = "double_option")]
    server_id: Option<Option<String>>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    password: Option<String>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

pub(super) async fn update(
    Extension(auth): Extension<ApiAuth>,
    Path(username): Path<String>,
    Json(req): Json<UpdateReq>,
) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let server_id = match req.server_id {
        Some(raw) => match parse_server_id(raw.as_deref()) {
            Ok(server_id) => Some(server_id),
            Err(message) => return bad_request(message),
        },
        None => None,
    };
    match account::update(
        &username,
        account::Update {
            privileged: req.privileged,
            server_id,
            disabled: req.disabled,
            password: req.password,
        },
    ) {
        Ok(account) => no_store(Json(account.view())),
        Err(message) => bad_request(message),
    }
}

pub(super) async fn remove(
    Extension(auth): Extension<ApiAuth>,
    Path(username): Path<String>,
) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    match account::delete(&username) {
        Ok(()) => no_store(Json(json!({ "deleted": true }))),
        Err(message) => bad_request(message),
    }
}

// An empty string clears the binding too, because that is what a form field a person emptied
// sends and refusing it would leave no way to unbind an account from the console.
fn parse_server_id(raw: Option<&str>) -> Result<Option<u64>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| "server_id must be a numeric Discord snowflake, as a string".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The console sends the field it is told to send, and the three shapes have to stay distinct:
    // absent leaves the binding alone (handled by the caller), empty and null clear it, and a
    // snowflake sets it. A snowflake read as an f64 would round.
    #[test]
    fn a_server_id_arrives_as_a_string_and_empties_to_none() {
        assert_eq!(parse_server_id(Some("1035861234567891234")), Ok(Some(1035861234567891234)));
        assert_eq!(parse_server_id(Some("  42 ")), Ok(Some(42)));
        assert_eq!(parse_server_id(Some("")), Ok(None));
        assert_eq!(parse_server_id(None), Ok(None));
        assert!(parse_server_id(Some("not-a-snowflake")).is_err());
    }
}
