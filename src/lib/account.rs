// Console accounts: a username, a password, and a privilege field.
//
// An API token is a secret that is pasted, shared, and pasted again — it identifies a piece of
// work, never a person. Everything the web console wants to do with an identity (show one user
// only their own jobs, hand one user the Users page, take a privilege away without breaking a
// script) needs a person, so a person is what this file stores. Tokens are unchanged and keep
// working; an account is enrolled *from* a token and inherits the reach that token had.
//
// Two files, because they have different lifetimes: `accounts.json` is the roster, `sessions.json`
// is who is currently signed in. Revoking the token somebody enrolled from must not sign them out,
// and signing everybody out must not delete anybody.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::lib::env::standard::{ACCOUNTS_PATH, SESSIONS_PATH};
use crate::lib::secret::{hex_bytes, random_hex_token};

// OWASP's PBKDF2-HMAC-SHA256 figure. The count is stored in every hash, so raising it here
// re-stretches each password the next time its owner sets one without invalidating the rest.
const ITERATIONS: u32 = 210_000;
const HASH_PREFIX: &str = "pbkdf2-sha256";
// A month. Long enough that a console nobody signs out of stays usable, short enough that a
// forgotten browser stops being a key eventually.
const SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;

pub const MIN_PASSWORD_LEN: usize = 10;
pub const MAX_PASSWORD_LEN: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub username: String,
    // The password's PBKDF2 record. It never leaves this module: `view` builds the API's shape by
    // naming fields rather than skipping them, so a field added later cannot leak by omission.
    pub password: String,
    #[serde(default)]
    pub privileged: bool,
    // Inherited from the token the account was enrolled from. A local token makes a server-scoped
    // account; a plain token makes one that sees only its own work.
    #[serde(default)]
    pub server_id: Option<u64>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_login: u64,
    // The label of the token this account was enrolled from, kept so the Users page can say where
    // somebody came from. It is a note, never an authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_from: Option<String>,
}

impl Account {
    // What the API hands back. Built by naming every field so the password record can never be
    // serialised by accident.
    pub fn view(&self) -> Value {
        json!({
            "username": self.username,
            "privileged": self.privileged,
            "server_id": self.server_id.map(|id| id.to_string()),
            "disabled": self.disabled,
            "created_at": self.created_at,
            "last_login": self.last_login,
            "enrolled_from": self.enrolled_from,
            "reach": self.reach(),
        })
    }

    // How much of the pipeline this account can see. The same three answers a token gets, so the
    // console renders one identity model rather than two.
    pub fn reach(&self) -> &'static str {
        if self.privileged {
            "everything"
        } else if self.server_id.is_some() {
            "server"
        } else {
            "own"
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
    token: String,
    username: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    expires_at: u64,
}

#[derive(Default)]
struct Store {
    loaded: bool,
    accounts: HashMap<String, Account>,
    sessions: HashMap<String, Session>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ensure_loaded(state: &mut Store) {
    if state.loaded {
        return;
    }
    state.loaded = true;
    if let Ok(contents) = std::fs::read_to_string(ACCOUNTS_PATH) {
        match serde_json::from_str::<Vec<Account>>(&contents) {
            Ok(accounts) => {
                for account in accounts {
                    state.accounts.insert(account.username.clone(), account);
                }
            }
            // Starting empty here would look like "no accounts exist", which the console reads as
            // an open deployment. Saying so is the only safe answer to a file we cannot parse.
            Err(error) => eprintln!("[accounts] {ACCOUNTS_PATH} is unreadable ({error}); no account can sign in until it is fixed"),
        }
    }
    if let Ok(contents) = std::fs::read_to_string(SESSIONS_PATH) {
        if let Ok(sessions) = serde_json::from_str::<Vec<Session>>(&contents) {
            let now = now();
            for session in sessions {
                if session.expires_at > now {
                    state.sessions.insert(session.token.clone(), session);
                }
            }
        }
    }
}

fn write_json(path: &str, body: String) {
    let path = std::path::Path::new(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Replaced by rename rather than rewritten in place: a truncated write to the account roster
    // locks every person out of the console at once.
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, body).is_err() {
        return;
    }
    restrict(&temporary);
    if std::fs::rename(&temporary, path).is_err() {
        std::fs::remove_file(&temporary).ok();
    }
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

fn save_accounts(state: &Store) {
    let mut accounts = state.accounts.values().cloned().collect::<Vec<_>>();
    accounts.sort_by(|a, b| a.username.cmp(&b.username));
    if let Ok(body) = serde_json::to_string_pretty(&accounts) {
        write_json(ACCOUNTS_PATH, body);
    }
}

fn save_sessions(state: &Store) {
    let mut sessions = state.sessions.values().cloned().collect::<Vec<_>>();
    sessions.sort_by(|a, b| a.username.cmp(&b.username).then(a.created_at.cmp(&b.created_at)));
    if let Ok(body) = serde_json::to_string_pretty(&sessions) {
        write_json(SESSIONS_PATH, body);
    }
}

// ---- passwords -------------------------------------------------------------

// HMAC-SHA256 by hand rather than by dependency: `sha2` is already here for the torrent and
// keyvault paths, and this is the whole of the construction.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0u8; BLOCK];
    let mut outer = [0u8; BLOCK];
    for index in 0..BLOCK {
        inner[index] = padded[index] ^ 0x36;
        outer[index] = padded[index] ^ 0x5c;
    }
    let mut hasher = Sha256::new();
    hasher.update(inner);
    hasher.update(message);
    let inner_digest = hasher.finalize();
    let mut hasher = Sha256::new();
    hasher.update(outer);
    hasher.update(inner_digest);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    out
}

// PBKDF2 with a derived key exactly one hash long, so there is a single block and its index is
// always 1. Anything wider would need the concatenation loop and buys nothing here.
fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut message = Vec::with_capacity(salt.len() + 4);
    message.extend_from_slice(salt);
    message.extend_from_slice(&1u32.to_be_bytes());
    let mut block = hmac_sha256(password, &message);
    let mut out = block;
    for _ in 1..iterations.max(1) {
        block = hmac_sha256(password, &block);
        for index in 0..32 {
            out[index] ^= block[index];
        }
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

// `$`-separated so the iteration count travels with the hash: raising `ITERATIONS` must not make
// every stored password unverifiable.
pub fn hash_password(password: &str) -> Result<String, String> {
    hash_password_with(password, ITERATIONS)
}

fn hash_password_with(password: &str, iterations: u32) -> Result<String, String> {
    let salt = random_hex_token().map_err(|e| format!("entropy source failed: {}", e))?;
    let salt = &salt[..32];
    let derived = pbkdf2(password.as_bytes(), salt.as_bytes(), iterations);
    Ok(format!("{}${}${}${}", HASH_PREFIX, iterations, salt, hex_bytes(&derived)))
}

// Constant-time in the comparison, because a password check that returns early tells an attacker
// how much of a guess was right.
fn equal_in_constant_time(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    if parts.next() != Some(HASH_PREFIX) {
        return false;
    }
    let Some(iterations) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
        return false;
    };
    let Some(salt) = parts.next() else { return false };
    let Some(expected) = parts.next().and_then(unhex) else {
        return false;
    };
    let derived = pbkdf2(password.as_bytes(), salt.as_bytes(), iterations);
    equal_in_constant_time(&derived, &expected)
}

// ---- names -----------------------------------------------------------------

// Lowercase because a roster where `Aya` and `aya` are two people is a roster nobody can
// administer, and because the Users page addresses an account by its name in a URL path.
pub fn normalize_username(raw: &str) -> Result<String, String> {
    let name = raw.trim().to_ascii_lowercase();
    if name.len() < 3 || name.len() > 32 {
        return Err("a username is 3 to 32 characters".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
        return Err("a username may use letters, digits, and . _ - only".to_string());
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("a username starts with a letter or a digit".to_string());
    }
    Ok(name)
}

pub fn check_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!("a password is at least {} characters", MIN_PASSWORD_LEN));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err("that password is too long".to_string());
    }
    Ok(())
}

// ---- roster ----------------------------------------------------------------

pub fn any_accounts() -> bool {
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    !state.accounts.is_empty()
}

pub fn list() -> Vec<Account> {
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    let mut accounts = state.accounts.values().cloned().collect::<Vec<_>>();
    accounts.sort_by(|a, b| a.username.cmp(&b.username));
    accounts
}

pub fn get(username: &str) -> Option<Account> {
    let name = normalize_username(username).ok()?;
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    state.accounts.get(&name).cloned()
}

pub fn create(
    username: &str,
    password: &str,
    privileged: bool,
    server_id: Option<u64>,
    enrolled_from: Option<String>,
) -> Result<Account, String> {
    let name = normalize_username(username)?;
    check_password(password)?;
    let hash = hash_password(password)?;
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    if state.accounts.contains_key(&name) {
        return Err("that username is taken".to_string());
    }
    let account = Account {
        username: name.clone(),
        password: hash,
        privileged,
        server_id,
        disabled: false,
        created_at: now(),
        last_login: 0,
        enrolled_from,
    };
    state.accounts.insert(name, account.clone());
    save_accounts(&state);
    Ok(account)
}

// Signing in. A disabled account is refused with the same words a wrong password gets, so the
// form cannot be used to enumerate who exists here.
pub fn authenticate(username: &str, password: &str) -> Result<Account, String> {
    const REFUSED: &str = "wrong username or password";
    let Ok(name) = normalize_username(username) else {
        return Err(REFUSED.to_string());
    };
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    let Some(account) = state.accounts.get(&name).cloned() else {
        return Err(REFUSED.to_string());
    };
    if !verify_password(password, &account.password) || account.disabled {
        return Err(REFUSED.to_string());
    }
    if let Some(entry) = state.accounts.get_mut(&name) {
        entry.last_login = now();
    }
    save_accounts(&state);
    Ok(account)
}

pub struct Update {
    pub privileged: Option<bool>,
    pub server_id: Option<Option<u64>>,
    pub disabled: Option<bool>,
    pub password: Option<String>,
}

// Every change the Users page can make, applied together. The two lockout guards live here rather
// than in the route so that no future caller can reach around them: an operator must not be able
// to remove the last privileged account, by demoting it or by disabling it.
pub fn update(username: &str, change: Update) -> Result<Account, String> {
    let name = normalize_username(username)?;
    let hash = match change.password.as_deref() {
        Some(password) => {
            check_password(password)?;
            Some(hash_password(password)?)
        }
        None => None,
    };
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    if !state.accounts.contains_key(&name) {
        return Err("no such account".to_string());
    }
    let losing_privilege = change.privileged == Some(false) || change.disabled == Some(true);
    if losing_privilege && is_last_privileged(&state, &name) {
        return Err("this is the last privileged account; promote another one first".to_string());
    }
    let entry = state.accounts.get_mut(&name).expect("checked above");
    if let Some(privileged) = change.privileged {
        entry.privileged = privileged;
    }
    if let Some(server_id) = change.server_id {
        entry.server_id = server_id;
    }
    if let Some(disabled) = change.disabled {
        entry.disabled = disabled;
    }
    if let Some(hash) = hash {
        entry.password = hash;
    }
    let updated = entry.clone();
    let sign_out = change.password.is_some() || change.disabled == Some(true);
    save_accounts(&state);
    if sign_out {
        // A new password or a disabled account has to reach the browsers that are already signed
        // in, and a session is exactly what would otherwise outlive it.
        state.sessions.retain(|_, session| session.username != name);
        save_sessions(&state);
    }
    Ok(updated)
}

pub fn delete(username: &str) -> Result<(), String> {
    let name = normalize_username(username)?;
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    if !state.accounts.contains_key(&name) {
        return Err("no such account".to_string());
    }
    if is_last_privileged(&state, &name) {
        return Err("this is the last privileged account; promote another one first".to_string());
    }
    state.accounts.remove(&name);
    state.sessions.retain(|_, session| session.username != name);
    save_accounts(&state);
    save_sessions(&state);
    Ok(())
}

fn is_last_privileged(state: &Store, name: &str) -> bool {
    let is_privileged = state
        .accounts
        .get(name)
        .map(|account| account.privileged && !account.disabled)
        .unwrap_or(false);
    if !is_privileged {
        return false;
    }
    state
        .accounts
        .values()
        .filter(|account| account.privileged && !account.disabled)
        .count()
        <= 1
}

// ---- sessions --------------------------------------------------------------

pub fn open_session(username: &str) -> Result<(String, u64), String> {
    let name = normalize_username(username)?;
    let token = random_hex_token().map_err(|e| format!("entropy source failed: {}", e))?;
    let created_at = now();
    let expires_at = created_at + SESSION_TTL_SECS;
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    state.sessions.retain(|_, session| session.expires_at > created_at);
    state.sessions.insert(
        token.clone(),
        Session { token: token.clone(), username: name, created_at, expires_at },
    );
    save_sessions(&state);
    Ok((token, expires_at))
}

// The account behind a session token, or None for one that expired, was signed out, or names an
// account that has since been deleted or disabled.
pub fn session_account(token: &str) -> Option<Account> {
    if token.is_empty() {
        return None;
    }
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    let session = state.sessions.get(token)?.clone();
    if session.expires_at <= now() {
        state.sessions.remove(token);
        save_sessions(&state);
        return None;
    }
    state
        .accounts
        .get(&session.username)
        .filter(|account| !account.disabled)
        .cloned()
}

pub fn close_session(token: &str) -> bool {
    let mut state = store().lock().unwrap();
    ensure_loaded(&mut state);
    let removed = state.sessions.remove(token).is_some();
    if removed {
        save_sessions(&state);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    // The stretch is deliberately expensive, so the tests prove the format and the comparison at a
    // count that costs nothing. `verify_password` reads the count out of the record, which is the
    // property that lets `ITERATIONS` be raised later without stranding stored passwords.
    #[test]
    fn a_hash_verifies_only_the_password_that_made_it() {
        let stored = hash_password_with("correct horse battery", 64).unwrap();
        assert!(stored.starts_with("pbkdf2-sha256$64$"));
        assert!(verify_password("correct horse battery", &stored));
        assert!(!verify_password("correct horse batter", &stored));
        assert!(!verify_password("", &stored));
    }

    // Two hashes of one password must differ, or the file tells a reader which accounts share one.
    #[test]
    fn the_same_password_hashes_differently_every_time() {
        let first = hash_password_with("correct horse battery", 64).unwrap();
        let second = hash_password_with("correct horse battery", 64).unwrap();
        assert_ne!(first, second);
        assert!(verify_password("correct horse battery", &second));
    }

    // A record this build cannot parse must fail closed rather than matching anything.
    #[test]
    fn a_malformed_record_verifies_nothing() {
        assert!(!verify_password("anything", ""));
        assert!(!verify_password("anything", "plaintext"));
        assert!(!verify_password("anything", "pbkdf2-sha256$notanumber$aa$bb"));
        assert!(!verify_password("anything", "argon2$1$aa$bb"));
    }

    // RFC 6070's PBKDF2-HMAC-SHA1 vectors do not apply here, so the check is against the SHA256
    // variant's published first vector: P = "password", S = "salt", c = 1.
    #[test]
    fn pbkdf2_matches_the_published_vector() {
        let derived = pbkdf2(b"password", b"salt", 1);
        assert_eq!(
            hex_bytes(&derived),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
        );
        let derived = pbkdf2(b"password", b"salt", 2);
        assert_eq!(
            hex_bytes(&derived),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43",
        );
    }

    // HMAC's own vector, since every iteration above is one of these and a wrong key-padding rule
    // would still produce a self-consistent — and completely wrong — hash.
    #[test]
    fn hmac_matches_the_published_vector() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex_bytes(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        );
        // A key longer than the 64-byte block is hashed first; getting that branch wrong is
        // invisible until somebody picks a long password.
        let mac = hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(
            hex_bytes(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        );
    }

    #[test]
    fn usernames_are_lowercased_and_bounded() {
        assert_eq!(normalize_username("  Aya  ").unwrap(), "aya");
        assert_eq!(normalize_username("mini-osaka_2.1").unwrap(), "mini-osaka_2.1");
        assert!(normalize_username("ay").is_err());
        assert!(normalize_username("has space").is_err());
        assert!(normalize_username("-leading").is_err());
        assert!(normalize_username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn a_short_password_is_refused() {
        assert!(check_password("123456789").is_err());
        assert!(check_password("1234567890").is_ok());
    }

    // The API's account shape must never carry the password record, whatever is added to the
    // struct later.
    #[test]
    fn the_api_view_omits_the_password() {
        let account = Account {
            username: "aya".to_string(),
            password: "pbkdf2-sha256$64$aa$bb".to_string(),
            privileged: false,
            server_id: Some(42),
            disabled: false,
            created_at: 1,
            last_login: 2,
            enrolled_from: Some("desk".to_string()),
        };
        let view = account.view();
        assert!(view.get("password").is_none());
        assert!(!view.to_string().contains("pbkdf2"));
        // Snowflakes exceed JS's safe integer range everywhere else in this API too.
        assert_eq!(view["server_id"], json!("42"));
        assert_eq!(view["reach"], json!("server"));
    }

    #[test]
    fn reach_follows_privilege_then_server() {
        let mut account = Account {
            username: "aya".to_string(),
            password: String::new(),
            privileged: false,
            server_id: None,
            disabled: false,
            created_at: 0,
            last_login: 0,
            enrolled_from: None,
        };
        assert_eq!(account.reach(), "own");
        account.server_id = Some(7);
        assert_eq!(account.reach(), "server");
        account.privileged = true;
        assert_eq!(account.reach(), "everything");
    }
}
