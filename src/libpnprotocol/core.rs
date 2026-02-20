use std::collections::HashMap;

//
// -----------------------------
// Errors
// -----------------------------
//

#[derive(Debug)]
pub enum ProtocolError {
    InvalidNegotiation,
    NotNegotiationLine,
    UnknownNegKey,
    NegotiationMalformed,
    ParseError,
}

//
// -----------------------------
// Runtime Data Model
// -----------------------------
//

#[derive(Debug, Clone)]
pub enum TypeC {
    Single(Data),
    Multi(Vec<TypeC>),
}

#[derive(Debug, Clone)]
pub struct Data {
    pub value: String,
}

//
// -----------------------------
// Schema Model
// -----------------------------
//

#[derive(Debug, Clone)]
pub enum Schema {
    Leaf,
    Multi(Vec<Schema>),
}

//
// -----------------------------
// Negotiation Model
// -----------------------------
//

#[derive(Debug, Clone)]
pub struct Negotiation {
    pub tool: String,
    pub build: String,
    pub grammar_version: u8,
}

//
// -----------------------------
// Protocol Core
// -----------------------------
//

pub struct Protocol {
    pub supported: Vec<u8>,
    pub negotiated: HashMap<String, Negotiation>,
}

impl Protocol {

    pub fn new(supported: Vec<u8>) -> Self {
        Self {
            supported,
            negotiated: HashMap::new(),
        }
    }

    //
    // -------------------------
    // Negotiation Parsing
    // -------------------------
    //

    pub fn negotiate(&mut self, neg_str: &str) -> Result<(), ProtocolError> {

        let parts: Vec<&str> = neg_str.split(':').collect();

        if !parts[0].starts_with("PNprotocol") {
            return Err(ProtocolError::NotNegotiationLine);
        }
        if parts.len() < 3 {
            return Err(ProtocolError::InvalidNegotiation);
        }

        let tool_part = parts[2];
        let neg_key = parts.last().unwrap();

        let tool_parts: Vec<&str> = tool_part.split('@').collect();

        if tool_parts.len() != 3 {
            return Err(ProtocolError::InvalidNegotiation);
        }

        let tool = tool_parts[0].to_string();
        let build = tool_parts[1].to_string();
        let grammar_version: u8 = tool_parts[2]
            .parse()
            .map_err(|_| ProtocolError::InvalidNegotiation)?;

        if !self.supported.contains(&grammar_version) {
            return Err(ProtocolError::InvalidNegotiation);
        }

        self.negotiated.insert(
            neg_key.to_string(),
            Negotiation {
                tool,
                build,
                grammar_version,
            },
        );

        Ok(())
    }

    //
    // -------------------------
    // Info String Parsing
    // -------------------------
    //

    pub fn extract_data(&self, data_str: &str) -> Result<TypeC, ProtocolError> {

        let mut split = data_str.splitn(2, ':');

        let negkey = split.next().ok_or(ProtocolError::ParseError)?;
        let payload = split.next().ok_or(ProtocolError::ParseError)?;

        let negotiation = self
            .negotiated
            .get(negkey)
            .ok_or(ProtocolError::UnknownNegKey)?;

        match negotiation.grammar_version {
            _ => parse_v1(payload)
        }
    }

    //
    // -------------------------
    // Info String Builder
    // -------------------------
    //

    pub fn build_info_string(
        &self,
        negkey: &str,
        schema: &Schema,
        data: &TypeC,
    ) -> Result<String, ProtocolError> {

        let negotiation = self
            .negotiated
            .get(negkey)
            .ok_or(ProtocolError::UnknownNegKey)?;

        let payload = match negotiation.grammar_version {
            _ => serialize_v1(schema, data)?
        };

        Ok(format!("{}:{}", negkey, payload))
    }

    pub fn request(
        &mut self,
        sender: ToolInfo,
        target: ToolInfo,
        key: String,
    ) -> String {

        let string = format!(
            "PNprotocol:{}@{}@{}:{}@{}@{}:{}",
            sender.tool,
            sender.build,
            sender.proto,
            target.tool,
            target.build,
            target.proto,
            key
        );
        self.negotiate(&string).unwrap();
        println!("{}", string);
        key
    }

}

//
// -----------------------------
// Escape / Unescape
// -----------------------------
//
// Escape encodes a raw leaf value so that the protocol delimiters
// (`:`, `/`, `%`) and the escape sentinel (`?`) cannot interfere with
// framing.
//
// Algorithm (escape):
//   1. Walk the string, strip every `?` and record its byte-offset in
//      the original string.
//   2. In the `?`-free string, replace each delimiter with its token:
//        `:`  →  `?PNcolon?`
//        `/`  →  `?PNslash?`
//        `%`  →  `?PNpercent?`
//      Build a cumulative offset-delta table so we can map positions
//      from the `?`-free string into the token-expanded string.
//   3. Map each saved `?`-index through that table, then insert
//      `?PNquestion?` at the adjusted positions (right-to-left so
//      earlier indices remain valid).
//
// Unescape reverses the three steps in the opposite order.
//

/// Escape a raw leaf value for safe embedding in a protocol frame.
pub fn escape(input: &str) -> String {
    // ── Step 1: strip `?`, remember byte-offsets in `input` ──────────
    let mut question_positions: Vec<usize> = Vec::new();
    let mut no_questions = String::with_capacity(input.len());

    for (i, ch) in input.char_indices() {
        if ch == '?' {
            question_positions.push(i);
        } else {
            no_questions.push(ch);
        }
    }

    // ── Step 2: replace delimiters, build offset-delta table ─────────
    //
    // offset_delta[i] = total extra bytes inserted at positions < i
    //                   in the `?`-free string (one entry per byte).
    let nq_len = no_questions.len();
    let mut offset_delta = vec![0usize; nq_len + 1];
    let mut accumulated: usize = 0;
    let mut escaped_delimiters = String::with_capacity(nq_len * 2);

    for (i, ch) in no_questions.char_indices() {
        offset_delta[i] = accumulated;
        match ch {
            ':' => {
                escaped_delimiters.push_str("?PNcolon?");
                accumulated += "?PNcolon?".len() - 1; // net extra bytes
            }
            '/' => {
                escaped_delimiters.push_str("?PNslash?");
                accumulated += "?PNslash?".len() - 1;
            }
            '%' => {
                escaped_delimiters.push_str("?PNpercent?");
                accumulated += "?PNpercent?".len() - 1;
            }
            _ => {
                escaped_delimiters.push(ch);
            }
        }
    }
    offset_delta[nq_len] = accumulated; // sentinel for end-of-string

    // ── Step 3: map `?`-positions, insert `?PNquestion?` ─────────────
    //
    // A `?` at byte `orig` in `input`:
    //   • `no_questions`-position = orig − (number of `?`s before it)
    //     (since each stripped `?` shifts subsequent positions by −1)
    //   • final position in `escaped_delimiters` = nq_pos + offset_delta[nq_pos]
    //
    // Collect and sort descending for right-to-left insertion.
    let token_q = "?PNquestion?";
    let mut insertions: Vec<usize> = question_positions
        .iter()
        .enumerate()
        .map(|(q_idx, &orig)| {
            let nq_pos = (orig - q_idx).min(nq_len);
            nq_pos + offset_delta[nq_pos]
        })
        .collect();

    insertions.sort_unstable_by(|a, b| b.cmp(a));

    for pos in insertions {
        escaped_delimiters.insert_str(pos, token_q);
    }

    escaped_delimiters
}

/// Unescape a protocol-framed leaf value back to its raw form.
pub fn unescape(input: &str) -> String {
    // ── Step 1: strip `?PNquestion?`, remember positions ─────────────
    let token_q  = "?PNquestion?";
    let token_c  = "?PNcolon?";
    let token_s  = "?PNslash?";
    let token_p  = "?PNpercent?";

    let mut question_positions: Vec<usize> = Vec::new();
    let mut no_q_tokens = String::with_capacity(input.len());

    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(token_q) {
            // position in the string being built at the moment of removal
            question_positions.push(no_q_tokens.len());
            i += token_q.len();
        } else {
            // SAFETY: we only advance one byte at a time for ASCII tokens;
            // all token chars are ASCII so this is fine.
            let ch = input[i..].chars().next().unwrap();
            no_q_tokens.push(ch);
            i += ch.len_utf8();
        }
    }

    // ── Step 2: replace tokens with delimiters, build shrink table ───
    //
    // offset_delta[i] = total bytes removed at positions < i in `no_q_tokens`.
    let nq_len = no_q_tokens.len();
    let mut offset_delta = vec![0usize; nq_len + 1];
    let mut shrunk: usize = 0;
    let mut restored = String::with_capacity(nq_len);
    let mut j = 0;

    while j < no_q_tokens.len() {
        offset_delta[j] = shrunk;
        if no_q_tokens[j..].starts_with(token_c) {
            restored.push(':');
            shrunk += token_c.len() - 1;
            j += token_c.len();
        } else if no_q_tokens[j..].starts_with(token_s) {
            restored.push('/');
            shrunk += token_s.len() - 1;
            j += token_s.len();
        } else if no_q_tokens[j..].starts_with(token_p) {
            restored.push('%');
            shrunk += token_p.len() - 1;
            j += token_p.len();
        } else {
            let ch = no_q_tokens[j..].chars().next().unwrap();
            restored.push(ch);
            j += ch.len_utf8();
        }
    }
    offset_delta[nq_len] = shrunk;

    // ── Step 3: map `?PNquestion?` positions, insert `?` ─────────────
    let mut insertions: Vec<usize> = question_positions
        .iter()
        .map(|&pos| {
            let clamped = pos.min(nq_len);
            clamped - offset_delta[clamped]
        })
        .collect();

    insertions.sort_unstable_by(|a, b| b.cmp(a));

    for pos in insertions {
        restored.insert(pos, '?');
    }

    restored
}

//
// -----------------------------
// Parsing Implementation
// -----------------------------
//

fn parse_v1(input: &str) -> Result<TypeC, ProtocolError> {
    let result = parse_level(input, ':', &['/', '%'])?;
    Ok(unescape_typec(result))
}

/// Recursively unescape all leaf values after structural parsing.
fn unescape_typec(tc: TypeC) -> TypeC {
    match tc {
        TypeC::Single(d) => TypeC::Single(Data { value: unescape(&d.value) }),
        TypeC::Multi(vec) => TypeC::Multi(vec.into_iter().map(unescape_typec).collect()),
    }
}

fn parse_level(
    input: &str,
    splitter: char,
    lower: &[char],
) -> Result<TypeC, ProtocolError> {

    let parts: Vec<&str> = input.split(splitter).collect();

    if parts.len() == 1 {

        if let Some((&next_split, remaining)) = lower.split_first() {
            if input.contains(next_split) {
                return parse_level(input, next_split, remaining);
            }
        }

        return Ok(TypeC::Single(Data {
            value: input.to_string(),
        }));
    }

    let mut vec = Vec::new();

    for part in parts {

        if let Some((&next_split, remaining)) = lower.split_first() {
            vec.push(parse_level(part, next_split, remaining)?);
        } else {
            vec.push(TypeC::Single(Data {
                value: part.to_string(),
            }));
        }
    }

    Ok(TypeC::Multi(vec))
}

//
// -----------------------------
// Serialization Implementation
// -----------------------------
//

fn serialize_v1(schema: &Schema, data: &TypeC) -> Result<String, ProtocolError> {
    // Escape leaf values before structural serialization so that any
    // `/`, `:`, `%`, or `?` in the raw data cannot break the framing.
    let escaped = escape_typec(data);
    serialize_level(schema, &escaped, ':', &['/', '%'])
}

/// Recursively escape all leaf values before structural serialization.
fn escape_typec(tc: &TypeC) -> TypeC {
    match tc {
        TypeC::Single(d) => TypeC::Single(Data { value: escape(&d.value) }),
        TypeC::Multi(vec) => TypeC::Multi(vec.iter().map(escape_typec).collect()),
    }
}

fn serialize_level(
    schema: &Schema,
    data: &TypeC,
    splitter: char,
    lower: &[char],
) -> Result<String, ProtocolError> {

    match (schema, data) {

        (Schema::Leaf, TypeC::Single(d)) => Ok(d.value.clone()),

        (Schema::Multi(schemas), TypeC::Multi(values)) => {

            if schemas.len() != values.len() {
                return Err(ProtocolError::NegotiationMalformed);
            }

            let mut parts = Vec::new();

            for (s, v) in schemas.iter().zip(values.iter()) {

                if let Some((&next_split, remaining)) = lower.split_first() {
                    parts.push(serialize_level(s, v, next_split, remaining)?);
                } else {
                    parts.push(serialize_level(s, v, splitter, &[])?);
                }
            }

            Ok(parts.join(&splitter.to_string()))
        }

        _ => Err(ProtocolError::NegotiationMalformed),
    }
}


//
// -----------------------------
// Example Usage
// -----------------------------
//
// let mut proto = Protocol::new(vec![3]);
//
// proto.negotiate("PNprotocol:PNprotocol@0.57@3:PNmpeg@0.11@3:ABC")
//     .unwrap();
//
// let built = pn_emit!(
//     protocol = proto,
//     negkey = "ABC",
//     schema = [leaf, [leaf, leaf]],
//     data   = ["hello", ["world", "42"]]
// ).unwrap();
//
// println!("{}", built);
//
// let parsed = proto.extract_data(&built).unwrap();
// println!("{:#?}", parsed);
//

#[derive(Clone)]
pub struct ToolInfo<'a> {
    pub tool: &'a str,
    pub build: &'a str,
    pub proto: u8,
}

//
// -----------------------------
// Tests
// -----------------------------
//

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape / unescape unit tests ─────────────────────────────────

    #[test]
    fn roundtrip_plain() {
        let s = "hello world";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_colon() {
        let s = "key:value";
        let e = escape(s);
        assert!(!e.contains(':'), "escaped colon must not remain: {}", e);
        assert_eq!(unescape(&e), s);
    }

    #[test]
    fn roundtrip_slash() {
        let s = "path/to/thing";
        let e = escape(s);
        assert!(!e.contains('/'));
        assert_eq!(unescape(&e), s);
    }

    #[test]
    fn roundtrip_percent() {
        let s = "100%done";
        let e = escape(s);
        assert!(!e.contains('%'));
        assert_eq!(unescape(&e), s);
    }

    #[test]
    fn roundtrip_question() {
        let s = "what?ever";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_question_token_like() {
        // A value that already looks like one of the tokens.
        let s = "?PNcolon?";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_all_special() {
        let s = "a:b/c%d?e?f:g";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_multiple_questions() {
        let s = "??::??";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_empty() {
        assert_eq!(unescape(&escape("")), "");
    }

    #[test]
    fn roundtrip_question_at_start_and_end() {
        let s = "?hello?";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn roundtrip_only_questions() {
        let s = "????";
        assert_eq!(unescape(&escape(s)), s);
    }

    // ── full protocol integration test ───────────────────────────────

    #[test]
    fn protocol_roundtrip_with_special_chars() {
        let mut proto = Protocol::new(vec![3]);
        proto.negotiate("PNprotocol:PNprotocol@0.57@3:PNmpeg@0.11@3:ABC").unwrap();

        let schema = Schema::Multi(vec![Schema::Leaf, Schema::Leaf]);
        let raw_a = "hello:world";
        let raw_b = "path/to/100%?done?";
        let data = TypeC::Multi(vec![
            TypeC::Single(Data { value: raw_a.to_string() }),
            TypeC::Single(Data { value: raw_b.to_string() }),
        ]);

        let built = proto.build_info_string("ABC", &schema, &data).unwrap();

        // The payload contains exactly one structural `:` separator between
        // the two leaves. Split on it and verify neither leaf-token contains
        // a bare delimiter (i.e. all special chars in the raw values are escaped).
        let payload = built.splitn(2, ':').nth(1).unwrap();
        let leaf_tokens: Vec<&str> = payload.splitn(2, ':').collect();
        assert_eq!(leaf_tokens.len(), 2, "expected exactly one structural separator");
        for token in &leaf_tokens {
            assert!(!token.contains(':'), "bare colon in leaf token: {}", token);
            assert!(!token.contains('/'), "bare slash in leaf token: {}", token);
            assert!(!token.contains('%'), "bare percent in leaf token: {}", token);
        }

        let parsed = proto.extract_data(&built).unwrap();
        if let TypeC::Multi(ref items) = parsed {
            assert_eq!(items.len(), 2);
            if let TypeC::Single(ref d) = items[0] { assert_eq!(d.value, raw_a); }
            else { panic!("expected Single for items[0]"); }
            if let TypeC::Single(ref d) = items[1] { assert_eq!(d.value, raw_b); }
            else { panic!("expected Single for items[1]"); }
        } else {
            panic!("expected Multi at top level");
        }
    }
}
