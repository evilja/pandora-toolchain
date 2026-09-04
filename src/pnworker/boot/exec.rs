// Runs a profile's steps in order against the provider. Everything is a structured `reqwest` call
// built from parsed values — there is no command line assembled anywhere in this file, and no body
// built by concatenating strings.
//
// The shape of a failure matters more than the shape of a success. A step that comes back with a
// status the profile does not accept is a plain failure: nothing happened, and the attempt can be
// retried after its cooldown. A step that times out, or whose connection drops, is *not* — the
// request may well have been received and acted on, so it ends the attempt as an unknown outcome
// that no automatic retry will touch.

use std::collections::BTreeMap;

use super::attempt;
use super::profile::{BootProfile, Step};

// A cap on what is read back from a provider. Responses are only ever read to extract a pointer or
// test a predicate, and a provider that streams a gigabyte at us is a provider that gets truncated.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub struct BootContext {
    pub attempt_id: String,
    pub node: String,
    // Set once by the caller and then only read: the values a profile may substitute.
    pub vars: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
}

pub enum StepOutcome {
    Completed,
    // The sequence stopped and nothing is owed to the provider.
    Failed(String),
    // A request was sent and its answer never arrived.
    Unknown(String),
}

// Runs every step. Returns as soon as one does not complete.
pub async fn run(profile: &BootProfile, ctx: &mut BootContext) -> StepOutcome {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(profile.file.boot_timeout_secs);
    let client = match reqwest::Client::builder()
        // A profile names its own endpoints; following a redirect to somewhere else would carry the
        // provider credential with it. A provider that answers 30x is a profile that should name
        // the real URL.
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => return StepOutcome::Failed(format!("could not build an HTTP client: {e}")),
    };

    for (index, step) in profile.file.steps.iter().enumerate() {
        let number = index + 1;
        attempt::set_step(&ctx.attempt_id, number, &step.id);
        if std::time::Instant::now() >= deadline {
            return StepOutcome::Failed(format!(
                "the boot timeout of {}s ran out before step {number} (`{}`)",
                profile.file.boot_timeout_secs, step.id
            ));
        }
        if step.delay_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(step.delay_secs)).await;
        }
        match run_step(&client, step, ctx, number, deadline).await {
            StepOutcome::Completed => {}
            other => return other,
        }
    }
    StepOutcome::Completed
}

async fn run_step(
    client: &reqwest::Client,
    step: &Step,
    ctx: &mut BootContext,
    number: usize,
    deadline: std::time::Instant,
) -> StepOutcome {
    let attempts = step.poll.as_ref().map(|p| p.max_attempts).unwrap_or(1);
    let interval = step.poll.as_ref().map(|p| p.interval_secs).unwrap_or(0);

    let mut last_seen = String::new();
    for round in 1..=attempts {
        if std::time::Instant::now() >= deadline {
            return StepOutcome::Failed(format!(
                "the boot timeout ran out while polling step {number} (`{}`)",
                step.id
            ));
        }
        let request = match build(client, step, ctx) {
            Ok(request) => request,
            // A build failure is a bad profile, not a provider problem: nothing was sent.
            Err(e) => {
                return StepOutcome::Failed(format!("step {number} (`{}`): {e}", step.id));
            }
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                // A connect error is safe to call a failure: the request never reached anyone. A
                // timeout, or a drop after the request went out, is not.
                if e.is_connect() {
                    return StepOutcome::Failed(format!(
                        "step {number} (`{}`) could not reach the provider: {}",
                        step.id,
                        sanitize(&e.to_string())
                    ));
                }
                return StepOutcome::Unknown(format!(
                    "step {number} (`{}`) was sent and no answer arrived ({}); the provider may have acted on it",
                    step.id,
                    sanitize(&e.to_string())
                ));
            }
        };
        let status = response.status();
        let body = read_body(response).await;
        if !accepted(step, status.as_u16()) {
            return StepOutcome::Failed(format!(
                "step {number} (`{}`) answered {}{}",
                step.id,
                status.as_u16(),
                summarize(&body)
            ));
        }

        let json = serde_json::from_str::<serde_json::Value>(&body).ok();

        // Captures run on every round so a polled step can extract the identifier it is polling on.
        if let Err(e) = capture(step, ctx, json.as_ref()) {
            return StepOutcome::Failed(format!("step {number} (`{}`): {e}", step.id));
        }

        let Some(poll) = &step.poll else {
            return StepOutcome::Completed;
        };
        let value = json
            .as_ref()
            .and_then(|j| pointer(j, &poll.pointer))
            .map(scalar)
            .unwrap_or_default();
        last_seen = value.clone();
        if poll.equals.iter().any(|want| want == &value) {
            return StepOutcome::Completed;
        }
        if poll.fails.iter().any(|bad| bad == &value) {
            return StepOutcome::Failed(format!(
                "step {number} (`{}`) reported `{}` at `{}`",
                step.id, value, poll.pointer
            ));
        }
        if round < attempts {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        }
    }

    StepOutcome::Failed(format!(
        "step {number} (`{}`) never reached {} — the last value was `{}`",
        step.id,
        step.poll
            .as_ref()
            .map(|p| p.equals.join(" or "))
            .unwrap_or_default(),
        last_seen
    ))
}

fn build(
    client: &reqwest::Client,
    step: &Step,
    ctx: &BootContext,
) -> Result<reqwest::RequestBuilder, String> {
    use super::profile::{expand, expand_json};

    let url = expand(&step.url, &ctx.vars, &ctx.secrets)?;
    // Re-checked after substitution: a captured value could otherwise turn a validated https URL
    // into something else entirely.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("the url did not resolve to an http(s) address".to_string());
    }
    let method = reqwest::Method::from_bytes(step.method.to_ascii_uppercase().as_bytes())
        .map_err(|_| format!("`{}` is not a method", step.method))?;
    let mut request = client
        .request(method, &url)
        .timeout(std::time::Duration::from_secs(step.timeout_secs));

    for (name, template) in &step.headers {
        let value = expand(template, &ctx.vars, &ctx.secrets)?;
        // The one injection this format could carry. A secret with a stray newline in it would
        // otherwise append a header of the attacker's choosing to every request built from it.
        if value.contains(['\n', '\r']) {
            return Err(format!("header `{name}` resolved to a value containing a newline"));
        }
        request = request.header(name.as_str(), value);
    }
    if let Some(auth) = &step.basic_auth {
        let username = expand(&auth.username, &ctx.vars, &ctx.secrets)?;
        let password = expand(&auth.password, &ctx.vars, &ctx.secrets)?;
        request = request.basic_auth(username, Some(password));
    }
    if !step.query.is_empty() {
        let mut pairs = Vec::new();
        for (name, template) in &step.query {
            pairs.push((
                expand(name, &ctx.vars, &ctx.secrets)?,
                expand(template, &ctx.vars, &ctx.secrets)?,
            ));
        }
        // `reqwest` percent-encodes these, so a value with an `&` in it stays one value.
        request = request.query(&pairs);
    }
    if let Some(body) = &step.json {
        request = request.json(&expand_json(body, &ctx.vars, &ctx.secrets)?);
    } else if !step.form.is_empty() {
        let mut pairs = Vec::new();
        for (name, template) in &step.form {
            pairs.push((
                expand(name, &ctx.vars, &ctx.secrets)?,
                expand(template, &ctx.vars, &ctx.secrets)?,
            ));
        }
        request = request.form(&pairs);
    }
    Ok(request)
}

fn accepted(step: &Step, status: u16) -> bool {
    if step.accept.is_empty() {
        (200..300).contains(&status)
    } else {
        step.accept.contains(&status)
    }
}

async fn read_body(response: reqwest::Response) -> String {
    match response.text().await {
        Ok(mut text) => {
            if text.len() > MAX_RESPONSE_BYTES {
                text.truncate(MAX_RESPONSE_BYTES);
            }
            text
        }
        Err(_) => String::new(),
    }
}

fn capture(
    step: &Step,
    ctx: &mut BootContext,
    json: Option<&serde_json::Value>,
) -> Result<(), String> {
    if step.capture.is_empty() {
        return Ok(());
    }
    let Some(json) = json else {
        return Err("the response was not JSON, so nothing could be captured from it".to_string());
    };
    for (name, ptr) in &step.capture {
        let value = pointer(json, ptr)
            .map(scalar)
            .ok_or_else(|| format!("the response has nothing at `{ptr}` to capture as `{name}`"))?;
        if value.is_empty() {
            return Err(format!("`{ptr}` is empty, so `{name}` would substitute nothing"));
        }
        ctx.vars.insert(format!("var.{name}"), value.clone());
        if step.provider_operation.as_deref() == Some(name.as_str()) {
            attempt::set_provider_operation(&ctx.attempt_id, &value);
        }
    }
    Ok(())
}

// An empty pointer is the whole document, which is what a provider answering a bare string or
// number needs.
fn pointer<'a>(json: &'a serde_json::Value, ptr: &str) -> Option<&'a serde_json::Value> {
    if ptr.is_empty() {
        Some(json)
    } else {
        json.pointer(ptr)
    }
}

// Scalars as their plain text, so a profile can compare against `running`, `3` or `true` without
// declaring a type. A structure has no scalar form and captures nothing.
fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

// Provider bodies reach the operator through `/lsnode` and the log, so they are trimmed to a line
// and capped. A provider that echoes a request back would otherwise put the credential it was sent
// into a status line.
fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let line = trimmed.lines().next().unwrap_or_default();
    let mut out: String = line.chars().take(160).collect();
    if line.chars().count() > 160 || trimmed.lines().count() > 1 {
        out.push('…');
    }
    format!(": {out}")
}

// A transport error's text can contain the URL, which can contain a query-string credential.
fn sanitize(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut skipping = false;
    for ch in message.chars() {
        if ch == '?' {
            skipping = true;
            out.push_str("?…");
            continue;
        }
        if skipping {
            if ch.is_whitespace() {
                skipping = false;
                out.push(ch);
            }
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pnworker::boot::profile::BootProfileFile;

    fn step(src: &str) -> Step {
        let file: BootProfileFile = toml::from_str(src).unwrap();
        file.steps.into_iter().next().unwrap()
    }

    #[test]
    fn an_empty_accept_list_means_any_2xx() {
        let s = step("[[steps]]\nid=\"a\"\nurl=\"https://x.example/\"\n");
        assert!(accepted(&s, 200));
        assert!(accepted(&s, 204));
        assert!(!accepted(&s, 302));
        assert!(!accepted(&s, 500));
    }

    #[test]
    fn an_explicit_accept_list_replaces_the_2xx_default() {
        let s = step("[[steps]]\nid=\"a\"\nurl=\"https://x.example/\"\naccept=[202,409]\n");
        assert!(accepted(&s, 202));
        assert!(accepted(&s, 409));
        // Deliberate: naming statuses means naming all of them, so a profile that accepts 409
        // cannot be surprised by a 200 it did not plan for.
        assert!(!accepted(&s, 200));
    }

    #[test]
    fn scalars_render_without_a_type_declaration() {
        assert_eq!(scalar(&serde_json::json!("running")), "running");
        assert_eq!(scalar(&serde_json::json!(3)), "3");
        assert_eq!(scalar(&serde_json::json!(true)), "true");
        assert_eq!(scalar(&serde_json::json!({"a":1})), "");
    }

    #[test]
    fn an_empty_pointer_is_the_whole_document() {
        let doc = serde_json::json!("running");
        assert_eq!(pointer(&doc, "").map(scalar).unwrap(), "running");
    }

    #[test]
    fn a_pointer_reads_a_nested_value() {
        let doc = serde_json::json!({"instance": {"id": "i-123"}});
        assert_eq!(pointer(&doc, "/instance/id").map(scalar).unwrap(), "i-123");
        assert!(pointer(&doc, "/instance/nope").is_none());
    }

    #[test]
    fn capture_records_the_value_for_later_steps() {
        let s = step(
            "[[steps]]\nid=\"a\"\nurl=\"https://x.example/\"\ncapture={ id = \"/instance/id\" }\n",
        );
        let mut ctx = BootContext {
            attempt_id: String::new(),
            node: "n".into(),
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
        };
        let doc = serde_json::json!({"instance": {"id": "i-123"}});
        capture(&s, &mut ctx, Some(&doc)).unwrap();
        assert_eq!(ctx.vars.get("var.id").map(String::as_str), Some("i-123"));
    }

    #[test]
    fn capturing_an_empty_value_fails_rather_than_substituting_nothing() {
        let s = step("[[steps]]\nid=\"a\"\nurl=\"https://x.example/\"\ncapture={ id = \"/id\" }\n");
        let mut ctx = BootContext {
            attempt_id: String::new(),
            node: "n".into(),
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
        };
        let err = capture(&s, &mut ctx, Some(&serde_json::json!({"id": ""}))).unwrap_err();
        assert!(err.contains("would substitute nothing"), "{err}");
    }

    #[test]
    fn capturing_from_a_non_json_response_is_an_error() {
        let s = step("[[steps]]\nid=\"a\"\nurl=\"https://x.example/\"\ncapture={ id = \"/id\" }\n");
        let mut ctx = BootContext {
            attempt_id: String::new(),
            node: "n".into(),
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
        };
        assert!(capture(&s, &mut ctx, None).is_err());
    }

    #[test]
    fn a_query_string_is_stripped_out_of_a_transport_error() {
        let message = "error sending request for url (https://x.example/a?key=SECRET)";
        let out = sanitize(message);
        assert!(!out.contains("SECRET"), "{out}");
        assert!(out.contains("https://x.example/a"), "{out}");
    }

    #[test]
    fn a_body_summary_is_one_capped_line() {
        let out = summarize(&format!("{}\nsecond line", "x".repeat(400)));
        assert!(out.len() < 200, "{}", out.len());
        assert!(!out.contains("second line"));
        assert!(out.ends_with('…'));
    }

    #[test]
    fn an_empty_body_summarizes_to_nothing() {
        assert_eq!(summarize("   "), "");
    }
}
