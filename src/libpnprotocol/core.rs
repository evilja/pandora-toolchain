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
// Parsing Implementation
// -----------------------------
//

fn parse_v1(input: &str) -> Result<TypeC, ProtocolError> {
    parse_level(input, ':', &['/', '%'])
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
    serialize_level(schema, data, ':', &['/', '%'])
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
