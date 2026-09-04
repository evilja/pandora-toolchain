// A boot profile: the ordered HTTP requests that bring one node's machine up, as an editable TOML
// file under `DB/config/global/boot-profiles/<id>.toml`. The file name without its extension is the
// profile id, which is what a binding stores and what `/gentoken boot:` returns.
//
// Everything here is declarative on purpose. There is no shell, no command field and no arbitrary
// path: a profile is a list of requests with headers and bodies, and the only thing a step can do
// beyond issuing one is capture a value out of its own response for a later step to substitute. A
// provider API that needs more than that gets an adapter in Rust rather than a wider file format,
// because the file is edited by hand and every capability added to it is one an operator can get
// wrong in a way nothing type-checks.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::lib::env::standard::LINK_BOOT_PROFILES_DIR;

// Ceilings rather than expected values. They exist so a mistyped `timeout_secs = 90000` is refused
// at parse time instead of holding a boot attempt open for a day.
pub const MAX_STEPS: usize = 32;
pub const MAX_STEP_TIMEOUT_SECS: u64 = 300;
pub const MAX_BOOT_TIMEOUT_SECS: u64 = 3600;
pub const MAX_POLL_ATTEMPTS: u32 = 240;
pub const MAX_DELAY_SECS: u64 = 300;

const DEFAULT_STEP_TIMEOUT_SECS: u64 = 30;
const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 600;
const DEFAULT_COOLDOWN_SECS: u64 = 900;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Debug, Deserialize)]
pub struct BootProfileFile {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_boot_timeout")]
    pub boot_timeout_secs: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub steps: Vec<Step>,
}

// What a machine started by this profile is expected to be able to do. It is a claim, not a proof:
// `expected` is what the scheduler may count on before the node exists, and the node's own measured
// encoder list at registration is what it is checked against. The image revision is what scopes
// that proof — a profile edited to rent different hardware invalidates what the old hardware
// proved, and without this field a stale proof would keep authorising boots for a machine that can
// no longer do the work.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    #[serde(default)]
    pub encoders: Vec<String>,
    #[serde(default)]
    pub image_revision: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default = "default_step_timeout")]
    pub timeout_secs: u64,
    // Seconds to wait before issuing this step. A provider that returns 200 from `start` and needs
    // a moment before its status endpoint exists is the ordinary case.
    #[serde(default)]
    pub delay_secs: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub json: Option<toml::Value>,
    #[serde(default)]
    pub form: BTreeMap<String, String>,
    #[serde(default)]
    pub basic_auth: Option<BasicAuth>,
    // Empty means "any 2xx". A provider that answers 202 for an accepted start, or 409 for "already
    // running", names those here rather than having the whole profile treat every non-2xx as fatal.
    #[serde(default)]
    pub accept: Vec<u16>,
    // JSON Pointer extractions out of this step's response body, by variable name. A missing
    // pointer fails the step: a captured value that silently became empty would be substituted into
    // the next request as an empty string, which is how a status poll ends up asking about instance
    // "" and getting a cheerful answer about nothing.
    #[serde(default)]
    pub capture: BTreeMap<String, String>,
    // The captured variable that holds this provider's operation or instance identifier. Recorded
    // on the attempt as soon as it is captured, because it is the only thing that can reconcile an
    // outcome nobody heard.
    #[serde(default)]
    pub provider_operation: Option<String>,
    #[serde(default)]
    pub poll: Option<Poll>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BasicAuth {
    pub username: String,
    #[serde(default)]
    pub password: String,
}

// Bounded polling for providers that provision asynchronously. The step's own request is repeated
// until the predicate holds or the attempts run out; there is no unbounded form, because a poll
// with no ceiling is a boot attempt that never reports anything an operator can act on.
#[derive(Clone, Debug, Deserialize)]
pub struct Poll {
    #[serde(default = "five")]
    pub interval_secs: u64,
    #[serde(default = "twelve")]
    pub max_attempts: u32,
    // The value to test, as a JSON Pointer into the response body.
    pub pointer: String,
    // One of these must match for the poll to succeed. Compared as strings against the pointed-at
    // value, so `true`, `3` and `"running"` all work without a type field.
    #[serde(default)]
    pub equals: Vec<String>,
    // Any of these ends the poll as a failure immediately rather than after `max_attempts` — a
    // provider that says `errored` is not going to say `running` sixty seconds later.
    #[serde(default)]
    pub fails: Vec<String>,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn five() -> u64 {
    5
}
fn twelve() -> u32 {
    12
}
fn get() -> String {
    "GET".to_string()
}
fn default_step_timeout() -> u64 {
    DEFAULT_STEP_TIMEOUT_SECS
}
fn default_boot_timeout() -> u64 {
    DEFAULT_BOOT_TIMEOUT_SECS
}
fn default_cooldown() -> u64 {
    DEFAULT_COOLDOWN_SECS
}
fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}

// A parsed, validated profile plus the identity and revision it was read under. The revision is the
// file's modification time: an attempt records the revision it started on, so a profile edited
// mid-boot does not retroactively change what is running, and a capability proof taken against an
// older revision can be told apart from one taken against this one.
#[derive(Clone, Debug)]
pub struct BootProfile {
    pub id: String,
    pub revision: u64,
    pub file: BootProfileFile,
}

impl BootProfile {
    pub fn display_name(&self) -> &str {
        if self.file.name.trim().is_empty() {
            &self.id
        } else {
            self.file.name.trim()
        }
    }
}

// A profile id is a file name, so it is checked as one. Anything that could climb out of the
// profiles directory or arrive with invisible edges is refused here rather than resolving to a file
// nobody meant to read.
pub fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn profile_path(id: &str) -> std::path::PathBuf {
    std::path::Path::new(LINK_BOOT_PROFILES_DIR).join(format!("{id}.toml"))
}

// Reads and validates one profile. Every failure is a string an operator can act on, because the
// only place these surface is a status line beside a node that did not start.
pub fn load(id: &str) -> Result<BootProfile, String> {
    if !valid_profile_id(id) {
        return Err(format!(
            "`{id}` is not a valid profile id (letters, digits, `-` and `_` only)"
        ));
    }
    let path = profile_path(id);
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let revision = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file: BootProfileFile =
        toml::from_str(&contents).map_err(|e| format!("{}: {e}", path.display()))?;
    validate(&file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(BootProfile {
        id: id.to_string(),
        revision,
        file,
    })
}

// Every profile in the directory, worst-first for the operator: a file that does not parse is
// returned as its error rather than skipped, so `/lsnode` can say a profile is broken instead of
// behaving as though it was never written.
pub fn load_all() -> Vec<Result<BootProfile, String>> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(LINK_BOOT_PROFILES_DIR) else {
        return out;
    };
    let mut ids = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !valid_profile_id(stem) {
            out.push(Err(format!(
                "{}: the file name is not a valid profile id",
                path.display()
            )));
            continue;
        }
        ids.push(stem.to_string());
    }
    ids.sort();
    for id in ids {
        out.push(load(&id));
    }
    out
}

pub fn validate(file: &BootProfileFile) -> Result<(), String> {
    if file.version != 1 {
        return Err(format!(
            "unsupported profile version {} (this build understands 1)",
            file.version
        ));
    }
    if file.steps.is_empty() {
        return Err("a profile needs at least one step".to_string());
    }
    if file.steps.len() > MAX_STEPS {
        return Err(format!(
            "{} steps is more than the {MAX_STEPS} a profile may have",
            file.steps.len()
        ));
    }
    if file.boot_timeout_secs == 0 || file.boot_timeout_secs > MAX_BOOT_TIMEOUT_SECS {
        return Err(format!(
            "boot_timeout_secs must be between 1 and {MAX_BOOT_TIMEOUT_SECS}"
        ));
    }
    let mut seen = std::collections::HashSet::new();
    // Variables a later step may substitute: what earlier steps captured, plus the values Pandora
    // supplies. Checked as the steps are walked so a `${var.x}` that reads a capture from a *later*
    // step is refused at parse time rather than failing on the machine at 3am.
    let mut known: std::collections::HashSet<String> =
        RESERVED_VARS.iter().map(|v| v.to_string()).collect();
    for (index, step) in file.steps.iter().enumerate() {
        let at = format!("step {} (`{}`)", index + 1, step.id);
        if step.id.trim().is_empty() {
            return Err(format!("step {} has no id", index + 1));
        }
        if !seen.insert(step.id.clone()) {
            return Err(format!("two steps share the id `{}`", step.id));
        }
        validate_method(&step.method).map_err(|e| format!("{at}: {e}"))?;
        validate_url_template(&step.url).map_err(|e| format!("{at}: {e}"))?;
        if step.timeout_secs == 0 || step.timeout_secs > MAX_STEP_TIMEOUT_SECS {
            return Err(format!(
                "{at}: timeout_secs must be between 1 and {MAX_STEP_TIMEOUT_SECS}"
            ));
        }
        if step.delay_secs > MAX_DELAY_SECS {
            return Err(format!("{at}: delay_secs may not exceed {MAX_DELAY_SECS}"));
        }
        if step.json.is_some() && !step.form.is_empty() {
            return Err(format!("{at}: a step sends either `json` or `form`, not both"));
        }
        for (name, value) in &step.headers {
            validate_header_name(name).map_err(|e| format!("{at}: {e}"))?;
            check_template(value, &known).map_err(|e| format!("{at}: header `{name}`: {e}"))?;
        }
        for (name, value) in &step.query {
            check_template(name, &known).map_err(|e| format!("{at}: query key: {e}"))?;
            check_template(value, &known).map_err(|e| format!("{at}: query `{name}`: {e}"))?;
        }
        for (name, value) in &step.form {
            check_template(name, &known).map_err(|e| format!("{at}: form key: {e}"))?;
            check_template(value, &known).map_err(|e| format!("{at}: form `{name}`: {e}"))?;
        }
        if let Some(body) = &step.json {
            check_toml_templates(body, &known).map_err(|e| format!("{at}: json: {e}"))?;
        }
        if let Some(auth) = &step.basic_auth {
            check_template(&auth.username, &known)
                .map_err(|e| format!("{at}: basic_auth username: {e}"))?;
            check_template(&auth.password, &known)
                .map_err(|e| format!("{at}: basic_auth password: {e}"))?;
        }
        check_template(&step.url, &known).map_err(|e| format!("{at}: url: {e}"))?;
        for status in &step.accept {
            if !(100..=599).contains(status) {
                return Err(format!("{at}: `{status}` is not an HTTP status"));
            }
        }
        if let Some(poll) = &step.poll {
            if poll.max_attempts == 0 || poll.max_attempts > MAX_POLL_ATTEMPTS {
                return Err(format!(
                    "{at}: poll.max_attempts must be between 1 and {MAX_POLL_ATTEMPTS}"
                ));
            }
            if poll.interval_secs == 0 || poll.interval_secs > MAX_DELAY_SECS {
                return Err(format!(
                    "{at}: poll.interval_secs must be between 1 and {MAX_DELAY_SECS}"
                ));
            }
            if poll.equals.is_empty() {
                return Err(format!(
                    "{at}: poll needs at least one `equals` value, or it can never succeed"
                ));
            }
            validate_pointer(&poll.pointer).map_err(|e| format!("{at}: poll.pointer: {e}"))?;
        }
        for (name, pointer) in &step.capture {
            validate_var_name(name).map_err(|e| format!("{at}: capture: {e}"))?;
            validate_pointer(pointer).map_err(|e| format!("{at}: capture `{name}`: {e}"))?;
        }
        if let Some(operation) = &step.provider_operation {
            if !step.capture.contains_key(operation) {
                return Err(format!(
                    "{at}: provider_operation names `{operation}`, which this step does not capture"
                ));
            }
        }
        for name in step.capture.keys() {
            known.insert(format!("var.{name}"));
        }
    }
    Ok(())
}

// Values Pandora substitutes into a profile. They are the node's own settings and the attempt's
// idempotency key — never a provider credential, which reaches a request through `${secret.*}` and
// is not exposed to the machine being started.
pub const RESERVED_VARS: &[&str] = &[
    "node.name",
    "node.token",
    "node.purpose",
    "node.coordinator_url",
    "attempt.id",
    "attempt.idempotency_key",
];

fn validate_method(method: &str) -> Result<(), String> {
    const ALLOWED: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];
    if ALLOWED.contains(&method.to_ascii_uppercase().as_str()) {
        Ok(())
    } else {
        Err(format!("`{method}` is not a method a profile may use"))
    }
}

// The scheme is checked on the literal prefix rather than after substitution, so a profile cannot
// be written whose URL scheme depends on a captured value. `http` is allowed because a provider
// reachable only on a private network is a real deployment; the check that matters is that it is
// one of the two and not `file:` or `unix:`.
fn validate_url_template(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("`{url}` must begin with https:// or http://"));
    }
    if url.contains(['\n', '\r']) {
        return Err("a url may not contain a newline".to_string());
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a header has no name".to_string());
    }
    // Deliberately stricter than the RFC's token rule: these are hand-written files, and every
    // character outside this set in a header name is a typo rather than an intention.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("`{name}` is not a valid header name"));
    }
    Ok(())
}

fn validate_var_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("`{name}` is not a valid variable name"));
    }
    Ok(())
}

fn validate_pointer(pointer: &str) -> Result<(), String> {
    if pointer.is_empty() {
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err(format!(
            "`{pointer}` must be a JSON Pointer beginning with `/`"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

// `${secret.name}` and `${var.name}` and the reserved `${node.*}` / `${attempt.*}` values. Anything
// else, and any unmatched brace, is an error rather than a literal: a profile that meant to name a
// variable and misspelled the prefix would otherwise send the braces to the provider verbatim.
pub fn check_template(
    template: &str,
    known: &std::collections::HashSet<String>,
) -> Result<(), String> {
    for reference in references(template)? {
        if let Some(name) = reference.strip_prefix("secret.") {
            validate_var_name(name)?;
            continue;
        }
        if !known.contains(&reference) {
            return Err(format!(
                "`${{{reference}}}` is not available here (captures can only be used by later steps)"
            ));
        }
    }
    Ok(())
}

fn check_toml_templates(
    value: &toml::Value,
    known: &std::collections::HashSet<String>,
) -> Result<(), String> {
    match value {
        toml::Value::String(s) => check_template(s, known),
        toml::Value::Array(items) => {
            for item in items {
                check_toml_templates(item, known)?;
            }
            Ok(())
        }
        toml::Value::Table(table) => {
            for (key, item) in table {
                check_template(key, known)?;
                check_toml_templates(item, known)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// The `${...}` references in a template, in order. A `$` not followed by `{` is a literal dollar,
// which is what a password generator produces often enough to matter.
pub fn references(template: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(end) = template[start..].find('}').map(|p| start + p) else {
                return Err("an unclosed `${` reference".to_string());
            };
            let name = template[start..end].trim().to_string();
            if name.is_empty() {
                return Err("an empty `${}` reference".to_string());
            }
            out.push(name);
            i = end + 1;
            continue;
        }
        i += 1;
    }
    Ok(out)
}

// Resolves every reference in one template. Values are substituted as plain strings into whatever
// structure holds them — a JSON string node, a query value, a header — and each of those is encoded
// by the thing that serialises it, so nothing here escapes and nothing here concatenates JSON.
pub fn expand(
    template: &str,
    vars: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(end) = template[start..].find('}').map(|p| start + p) else {
                return Err("an unclosed `${` reference".to_string());
            };
            let name = template[start..end].trim();
            let value = if let Some(secret) = name.strip_prefix("secret.") {
                secrets
                    .get(secret)
                    .ok_or_else(|| format!("no secret named `{secret}` is configured"))?
            } else {
                vars.get(name)
                    .ok_or_else(|| format!("`${{{name}}}` has no value"))?
            };
            out.push_str(value);
            i = end + 1;
            continue;
        }
        out.push(template[i..].chars().next().unwrap());
        i += template[i..].chars().next().unwrap().len_utf8();
    }
    Ok(out)
}

// A TOML body with every string expanded, as `serde_json::Value`. Converting the parsed tree rather
// than the source text is what keeps substitution from being string concatenation: a secret
// containing a quote lands in a JSON string node and is escaped when the node is serialised.
pub fn expand_json(
    value: &toml::Value,
    vars: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> Result<serde_json::Value, String> {
    Ok(match value {
        toml::Value::String(s) => serde_json::Value::String(expand(s, vars, secrets)?),
        toml::Value::Integer(i) => serde_json::Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| expand_json(item, vars, secrets))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (key, item) in table {
                map.insert(expand(key, vars, secrets)?, expand_json(item, vars, secrets)?);
            }
            serde_json::Value::Object(map)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn parse(src: &str) -> Result<BootProfileFile, String> {
        let file: BootProfileFile = toml::from_str(src).map_err(|e| e.to_string())?;
        validate(&file)?;
        Ok(file)
    }

    const MINIMAL: &str = r#"
version = 1
name = "GPU worker"
[[steps]]
id = "start"
method = "POST"
url = "https://provider.example/start"
"#;

    #[test]
    fn a_minimal_profile_parses() {
        let file = parse(MINIMAL).unwrap();
        assert_eq!(file.steps.len(), 1);
        assert!(file.enabled);
        assert_eq!(file.steps[0].method, "POST");
    }

    #[test]
    fn a_profile_with_no_steps_is_refused() {
        let err = parse("version = 1\nname = \"x\"\n").unwrap_err();
        assert!(err.contains("at least one step"), "{err}");
    }

    #[test]
    fn a_future_version_is_refused_rather_than_guessed_at() {
        let err = parse(&MINIMAL.replace("version = 1", "version = 2")).unwrap_err();
        assert!(err.contains("unsupported profile version"), "{err}");
    }

    #[test]
    fn a_non_http_url_is_refused() {
        let err = parse(&MINIMAL.replace("https://provider.example/start", "file:///etc/passwd"))
            .unwrap_err();
        assert!(err.contains("https://"), "{err}");
    }

    #[test]
    fn a_capture_cannot_be_used_by_the_step_that_makes_it() {
        let src = r#"
version = 1
[[steps]]
id = "start"
url = "https://provider.example/${var.id}"
capture = { id = "/id" }
"#;
        let err = parse(src).unwrap_err();
        assert!(err.contains("not available here"), "{err}");
    }

    #[test]
    fn a_later_step_may_use_an_earlier_capture() {
        let src = r#"
version = 1
[[steps]]
id = "start"
method = "POST"
url = "https://provider.example/start"
capture = { id = "/instance/id" }
provider_operation = "id"

[[steps]]
id = "status"
url = "https://provider.example/instances/${var.id}"
"#;
        parse(src).unwrap();
    }

    #[test]
    fn provider_operation_must_name_something_the_step_captures() {
        let src = r#"
version = 1
[[steps]]
id = "start"
method = "POST"
url = "https://provider.example/start"
provider_operation = "id"
"#;
        let err = parse(src).unwrap_err();
        assert!(err.contains("does not capture"), "{err}");
    }

    #[test]
    fn a_header_name_with_a_newline_is_refused() {
        let src = r#"
version = 1
[[steps]]
id = "start"
url = "https://provider.example/start"
[steps.headers]
"X-Bad\nInjected" = "1"
"#;
        let err = parse(src).unwrap_err();
        assert!(err.contains("not a valid header name"), "{err}");
    }

    #[test]
    fn a_step_may_not_send_both_json_and_form() {
        let src = r#"
version = 1
[[steps]]
id = "start"
method = "POST"
url = "https://provider.example/start"
json = { a = 1 }
form = { b = "2" }
"#;
        let err = parse(src).unwrap_err();
        assert!(err.contains("not both"), "{err}");
    }

    #[test]
    fn a_poll_with_no_equals_can_never_succeed_and_is_refused() {
        let src = r#"
version = 1
[[steps]]
id = "status"
url = "https://provider.example/status"
[steps.poll]
pointer = "/state"
"#;
        let err = parse(src).unwrap_err();
        assert!(err.contains("equals"), "{err}");
    }

    #[test]
    fn an_oversized_timeout_is_refused_at_parse_time() {
        let src = MINIMAL.replace("id = \"start\"", "id = \"start\"\ntimeout_secs = 90000");
        let err = parse(&src).unwrap_err();
        assert!(err.contains("timeout_secs"), "{err}");
    }

    #[test]
    fn expansion_fails_on_a_missing_variable_rather_than_emitting_an_empty_string() {
        let err = expand("${var.nope}", &vars(&[]), &vars(&[])).unwrap_err();
        assert!(err.contains("has no value"), "{err}");
    }

    #[test]
    fn expansion_fails_on_a_missing_secret() {
        let err = expand("${secret.nope}", &vars(&[]), &vars(&[])).unwrap_err();
        assert!(err.contains("no secret named"), "{err}");
    }

    #[test]
    fn a_bare_dollar_is_a_literal() {
        let out = expand("pa$$word", &vars(&[]), &vars(&[])).unwrap();
        assert_eq!(out, "pa$$word");
    }

    #[test]
    fn an_unclosed_reference_is_an_error() {
        assert!(expand("${var.a", &vars(&[]), &vars(&[])).is_err());
    }

    #[test]
    fn a_secret_containing_json_punctuation_is_escaped_by_the_serialiser() {
        let body: toml::Value = toml::from_str(r#"key = "${secret.k}""#).unwrap();
        let json = expand_json(&body, &vars(&[]), &vars(&[("k", "a\":1,\"b")])).unwrap();
        let text = serde_json::to_string(&json).unwrap();
        // One key, one string value: the quote inside the secret did not become structure.
        assert_eq!(text, r#"{"key":"a\":1,\"b"}"#);
        assert_eq!(json["key"], serde_json::Value::String("a\":1,\"b".into()));
    }

    #[test]
    fn expansion_preserves_non_string_json_types() {
        let body: toml::Value =
            toml::from_str("n = 3\nb = true\nnested = { s = \"${var.v}\" }").unwrap();
        let json = expand_json(&body, &vars(&[("var.v", "x")]), &vars(&[])).unwrap();
        assert_eq!(json["n"], serde_json::json!(3));
        assert_eq!(json["b"], serde_json::json!(true));
        assert_eq!(json["nested"]["s"], serde_json::json!("x"));
    }

    #[test]
    fn reserved_node_values_are_available_to_the_first_step() {
        let src = r#"
version = 1
[[steps]]
id = "start"
method = "POST"
url = "https://provider.example/start"
json = { name = "${node.name}", key = "${attempt.idempotency_key}" }
"#;
        parse(src).unwrap();
    }

    #[test]
    fn profile_ids_reject_traversal() {
        assert!(valid_profile_id("gpu-worker"));
        assert!(!valid_profile_id("../etc/passwd"));
        assert!(!valid_profile_id("gpu/worker"));
        assert!(!valid_profile_id(""));
    }
}
