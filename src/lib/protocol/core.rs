use std::collections::HashMap;

#[derive(Debug)]
pub enum ProtocolError {
    InvalidNegotiation,
    NotNegotiationLine,
    UnknownNegKey,
    NegotiationMalformed,
    ParseError,
}

#[derive(Debug, Clone)]
pub enum TypeC {
    Single(Data),
    Multi(Vec<TypeC>),
}

#[derive(Debug, Clone)]
pub struct Data {
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum Schema {
    Leaf,
    Multi(Vec<Schema>),
}

#[derive(Debug, Clone)]
pub struct Negotiation {
    pub tool: String,
    pub build: String,
    pub grammar_version: u8,
}

#[derive(Clone)]
pub struct ToolInfo<'a> {
    pub tool: &'a str,
    pub build: &'a str,
    pub proto: u8,
}

pub struct Protocol {
    pub supported: Vec<u8>,
    pub negotiated: HashMap<String, Negotiation>,
}

impl Protocol {
    pub fn new(supported: Vec<u8>) -> Self {
        Self { supported, negotiated: HashMap::new() }
    }

    pub fn negotiate(&mut self, neg_str: &str) -> Result<(), ProtocolError> {
        let parts: Vec<&str> = neg_str.split(':').collect();

        if !parts[0].starts_with("PNprotocol") {
            return Err(ProtocolError::NotNegotiationLine);
        }
        if parts.len() < 3 {
            return Err(ProtocolError::InvalidNegotiation);
        }

        let tool_parts: Vec<&str> = parts[2].split('@').collect();
        if tool_parts.len() != 3 {
            return Err(ProtocolError::InvalidNegotiation);
        }

        let grammar_version: u8 = tool_parts[2]
            .parse()
            .map_err(|_| ProtocolError::InvalidNegotiation)?;

        if !self.supported.contains(&grammar_version) {
            return Err(ProtocolError::InvalidNegotiation);
        }

        self.negotiated.insert(
            parts.last().unwrap().to_string(),
            Negotiation {
                tool: tool_parts[0].to_string(),
                build: tool_parts[1].to_string(),
                grammar_version,
            },
        );

        Ok(())
    }

    pub fn extract_data(&self, data_str: &str) -> Result<TypeC, ProtocolError> {
        let mut split = data_str.splitn(2, ':');
        let negkey = split.next().ok_or(ProtocolError::ParseError)?;
        let payload = split.next().ok_or(ProtocolError::ParseError)?;

        let negotiation = self.negotiated.get(negkey).ok_or(ProtocolError::UnknownNegKey)?;

        match negotiation.grammar_version {
            _ => parse_v1(payload)
        }
    }

    pub fn build_info_string(&self, negkey: &str, schema: &Schema, data: &TypeC) -> Result<String, ProtocolError> {
        let negotiation = self.negotiated.get(negkey).ok_or(ProtocolError::UnknownNegKey)?;

        let payload = match negotiation.grammar_version {
            _ => serialize_v1(schema, data)?
        };

        Ok(format!("{}:{}", negkey, payload))
    }

    pub fn request(&mut self, sender: ToolInfo, target: ToolInfo, key: String) -> String {
        let string = format!(
            "PNprotocol:{}@{}@{}:{}@{}@{}:{}",
            sender.tool, sender.build, sender.proto,
            target.tool, target.build, target.proto,
            key
        );
        self.negotiate(&string).unwrap();
        println!("{}", string);
        key
    }
}

impl TypeC {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            TypeC::Single(d) => Some(&d.value),
            _ => None,
        }
    }

    pub fn get(&self, index: usize) -> Option<&TypeC> {
        match self {
            TypeC::Multi(v) => v.get(index),
            _ => None,
        }
    }

    pub fn as_multi(&self) -> Option<&Vec<TypeC>> {
        match self {
            TypeC::Multi(v) => Some(v),
            _ => None,
        }
    }

    pub fn parse<T: std::str::FromStr>(&self) -> Option<T> {
        self.as_str()?.parse().ok()
    }
}

pub fn escape(input: &str) -> String {
    let mut question_positions: Vec<usize> = Vec::new();
    let mut no_questions = String::with_capacity(input.len());

    for (i, ch) in input.char_indices() {
        if ch == '?' {
            question_positions.push(i);
        } else {
            no_questions.push(ch);
        }
    }

    let nq_len = no_questions.len();
    let mut offset_delta = vec![0usize; nq_len + 1];
    let mut accumulated: usize = 0;
    let mut escaped_delimiters = String::with_capacity(nq_len * 2);

    for (i, ch) in no_questions.char_indices() {
        offset_delta[i] = accumulated;
        match ch {
            ':' => { escaped_delimiters.push_str("?PNcolon?");   accumulated += "?PNcolon?".len() - 1; }
            '/' => { escaped_delimiters.push_str("?PNslash?");   accumulated += "?PNslash?".len() - 1; }
            '%' => { escaped_delimiters.push_str("?PNpercent?"); accumulated += "?PNpercent?".len() - 1; }
            _   => { escaped_delimiters.push(ch); }
        }
    }
    offset_delta[nq_len] = accumulated;

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
        escaped_delimiters.insert_str(pos, "?PNquestion?");
    }

    escaped_delimiters
}

pub fn unescape(input: &str) -> String {
    let (token_q, token_c, token_s, token_p) = ("?PNquestion?", "?PNcolon?", "?PNslash?", "?PNpercent?");

    let mut question_positions: Vec<usize> = Vec::new();
    let mut no_q_tokens = String::with_capacity(input.len());

    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(token_q) {
            question_positions.push(no_q_tokens.len());
            i += token_q.len();
        } else {
            let ch = input[i..].chars().next().unwrap();
            no_q_tokens.push(ch);
            i += ch.len_utf8();
        }
    }

    let nq_len = no_q_tokens.len();
    let mut offset_delta = vec![0usize; nq_len + 1];
    let mut shrunk: usize = 0;
    let mut restored = String::with_capacity(nq_len);
    let mut j = 0;

    while j < no_q_tokens.len() {
        offset_delta[j] = shrunk;
        if no_q_tokens[j..].starts_with(token_c) {
            restored.push(':'); shrunk += token_c.len() - 1; j += token_c.len();
        } else if no_q_tokens[j..].starts_with(token_s) {
            restored.push('/'); shrunk += token_s.len() - 1; j += token_s.len();
        } else if no_q_tokens[j..].starts_with(token_p) {
            restored.push('%'); shrunk += token_p.len() - 1; j += token_p.len();
        } else {
            let ch = no_q_tokens[j..].chars().next().unwrap();
            restored.push(ch);
            j += ch.len_utf8();
        }
    }
    offset_delta[nq_len] = shrunk;

    let mut insertions: Vec<usize> = question_positions
        .iter()
        .map(|&pos| { let c = pos.min(nq_len); c - offset_delta[c] })
        .collect();

    insertions.sort_unstable_by(|a, b| b.cmp(a));
    for pos in insertions {
        restored.insert(pos, '?');
    }

    restored
}

fn parse_v1(input: &str) -> Result<TypeC, ProtocolError> {
    let result = parse_level(input, ':', &['/', '%'])?;
    Ok(unescape_typec(result))
}

fn unescape_typec(tc: TypeC) -> TypeC {
    match tc {
        TypeC::Single(d) => TypeC::Single(Data { value: unescape(&d.value) }),
        TypeC::Multi(vec) => TypeC::Multi(vec.into_iter().map(unescape_typec).collect()),
    }
}

fn parse_level(input: &str, splitter: char, lower: &[char]) -> Result<TypeC, ProtocolError> {
    let parts: Vec<&str> = input.split(splitter).collect();

    if parts.len() == 1 {
        if let Some((&next_split, remaining)) = lower.split_first() {
            if input.contains(next_split) {
                return parse_level(input, next_split, remaining);
            }
        }
        return Ok(TypeC::Single(Data { value: input.to_string() }));
    }

    let mut vec = Vec::new();
    for part in parts {
        if let Some((&next_split, remaining)) = lower.split_first() {
            vec.push(parse_level(part, next_split, remaining)?);
        } else {
            vec.push(TypeC::Single(Data { value: part.to_string() }));
        }
    }

    Ok(TypeC::Multi(vec))
}

fn serialize_v1(schema: &Schema, data: &TypeC) -> Result<String, ProtocolError> {
    let escaped = escape_typec(data);
    serialize_level(schema, &escaped, ':', &['/', '%'])
}

fn escape_typec(tc: &TypeC) -> TypeC {
    match tc {
        TypeC::Single(d) => TypeC::Single(Data { value: escape(&d.value) }),
        TypeC::Multi(vec) => TypeC::Multi(vec.iter().map(escape_typec).collect()),
    }
}

fn serialize_level(schema: &Schema, data: &TypeC, splitter: char, lower: &[char]) -> Result<String, ProtocolError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn roundtrip_plain() { let s = "hello world"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_colon() { let s = "key:value"; let e = escape(s); assert!(!e.contains(':')); assert_eq!(unescape(&e), s); }
    #[test] fn roundtrip_slash() { let s = "path/to/thing"; let e = escape(s); assert!(!e.contains('/')); assert_eq!(unescape(&e), s); }
    #[test] fn roundtrip_percent() { let s = "100%done"; let e = escape(s); assert!(!e.contains('%')); assert_eq!(unescape(&e), s); }
    #[test] fn roundtrip_question() { let s = "what?ever"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_question_token_like() { let s = "?PNcolon?"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_all_special() { let s = "a:b/c%d?e?f:g"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_multiple_questions() { let s = "??::??"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_empty() { assert_eq!(unescape(&escape("")), ""); }
    #[test] fn roundtrip_question_at_start_and_end() { let s = "?hello?"; assert_eq!(unescape(&escape(s)), s); }
    #[test] fn roundtrip_only_questions() { let s = "????"; assert_eq!(unescape(&escape(s)), s); }

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
        let payload = built.splitn(2, ':').nth(1).unwrap();
        let leaf_tokens: Vec<&str> = payload.splitn(2, ':').collect();
        assert_eq!(leaf_tokens.len(), 2);
        for token in &leaf_tokens {
            assert!(!token.contains(':'));
            assert!(!token.contains('/'));
            assert!(!token.contains('%'));
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
