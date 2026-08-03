use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::error::{Result, TorrentError};

const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bytes(Vec<u8>),
    Integer(i64),
    List(Vec<Value>),
    Dictionary(BTreeMap<Vec<u8>, Value>),
}

impl Value {
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_dictionary(&self) -> Option<&BTreeMap<Vec<u8>, Value>> {
        match self {
            Self::Dictionary(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<&Value> {
        self.as_dictionary()?.get(key)
    }
}

pub fn decode(data: &[u8]) -> Result<Value> {
    let (value, consumed) = decode_prefix(data)?;
    if consumed != data.len() {
        return Err(TorrentError::bencode("trailing bytes after the root value"));
    }
    Ok(value)
}

pub fn decode_prefix(data: &[u8]) -> Result<(Value, usize)> {
    let mut parser = Parser {
        data,
        position: 0,
        items: 0,
    };
    let value = parser.value(0)?;
    Ok((value, parser.position))
}

pub fn dictionary_value_range(data: &[u8], wanted: &[u8]) -> Result<Range<usize>> {
    if data.first() != Some(&b'd') {
        return Err(TorrentError::bencode("root value is not a dictionary"));
    }
    let mut parser = Parser {
        data,
        position: 1,
        items: 0,
    };
    let mut seen = BTreeSet::new();
    let mut found = None;
    while parser.peek()? != b'e' {
        let key = parser.bytes()?;
        if !seen.insert(key.clone()) {
            return Err(TorrentError::bencode("dictionary contains a duplicate key"));
        }
        let start = parser.position;
        parser.skip_value(1)?;
        if key == wanted {
            found = Some(start..parser.position);
        }
    }
    found.ok_or_else(|| {
        TorrentError::bencode(format!(
            "dictionary key {:?} is missing",
            String::from_utf8_lossy(wanted)
        ))
    })
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    encode_into(value, &mut output);
    output
}

fn encode_into(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Bytes(bytes) => {
            output.extend_from_slice(bytes.len().to_string().as_bytes());
            output.push(b':');
            output.extend_from_slice(bytes);
        }
        Value::Integer(integer) => {
            output.push(b'i');
            output.extend_from_slice(integer.to_string().as_bytes());
            output.push(b'e');
        }
        Value::List(values) => {
            output.push(b'l');
            for value in values {
                encode_into(value, output);
            }
            output.push(b'e');
        }
        Value::Dictionary(values) => {
            output.push(b'd');
            for (key, value) in values {
                encode_into(&Value::Bytes(key.clone()), output);
                encode_into(value, output);
            }
            output.push(b'e');
        }
    }
}

struct Parser<'a> {
    data: &'a [u8],
    position: usize,
    items: usize,
}

impl Parser<'_> {
    fn value(&mut self, depth: usize) -> Result<Value> {
        self.bump_item(depth)?;
        match self.peek()? {
            b'i' => self.integer().map(Value::Integer),
            b'l' => self.list(depth).map(Value::List),
            b'd' => self.dictionary(depth).map(Value::Dictionary),
            b'0'..=b'9' => self.bytes().map(Value::Bytes),
            byte => Err(TorrentError::bencode(format!(
                "unexpected byte 0x{byte:02x} at offset {}",
                self.position
            ))),
        }
    }

    fn skip_value(&mut self, depth: usize) -> Result<()> {
        self.bump_item(depth)?;
        match self.peek()? {
            b'i' => {
                self.integer()?;
            }
            b'l' => {
                self.position += 1;
                while self.peek()? != b'e' {
                    self.skip_value(depth + 1)?;
                }
                self.position += 1;
            }
            b'd' => {
                self.position += 1;
                let mut seen = BTreeSet::new();
                while self.peek()? != b'e' {
                    let key = self.bytes()?;
                    if !seen.insert(key) {
                        return Err(TorrentError::bencode("dictionary contains a duplicate key"));
                    }
                    self.skip_value(depth + 1)?;
                }
                self.position += 1;
            }
            b'0'..=b'9' => {
                self.bytes()?;
            }
            byte => {
                return Err(TorrentError::bencode(format!(
                    "unexpected byte 0x{byte:02x} at offset {}",
                    self.position
                )));
            }
        }
        Ok(())
    }

    fn bump_item(&mut self, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(TorrentError::bencode("nesting limit exceeded"));
        }
        self.items += 1;
        if self.items > MAX_ITEMS {
            return Err(TorrentError::bencode("item limit exceeded"));
        }
        Ok(())
    }

    fn peek(&self) -> Result<u8> {
        self.data
            .get(self.position)
            .copied()
            .ok_or_else(|| TorrentError::bencode("unexpected end of input"))
    }

    fn integer(&mut self) -> Result<i64> {
        let start = self.position;
        self.position += 1;
        let digits_start = self.position;
        while self.peek()? != b'e' {
            let byte = self.peek()?;
            if !(byte.is_ascii_digit() || (byte == b'-' && self.position == digits_start)) {
                return Err(TorrentError::bencode(format!(
                    "invalid integer at offset {start}"
                )));
            }
            self.position += 1;
        }
        let digits = &self.data[digits_start..self.position];
        self.position += 1;
        if digits.is_empty()
            || digits == b"-"
            || (digits.len() > 1 && digits[0] == b'0')
            || (digits.starts_with(b"-0"))
        {
            return Err(TorrentError::bencode(format!(
                "non-canonical integer at offset {start}"
            )));
        }
        let text = std::str::from_utf8(digits)
            .map_err(|_| TorrentError::bencode("integer is not ASCII"))?;
        text.parse::<i64>()
            .map_err(|_| TorrentError::bencode("integer is outside the i64 range"))
    }

    fn bytes(&mut self) -> Result<Vec<u8>> {
        let start = self.position;
        while self.peek()?.is_ascii_digit() {
            self.position += 1;
        }
        if self.peek()? != b':' {
            return Err(TorrentError::bencode(format!(
                "invalid byte string length at offset {start}"
            )));
        }
        let length_digits = &self.data[start..self.position];
        if length_digits.is_empty()
            || (length_digits.len() > 1 && length_digits.first() == Some(&b'0'))
        {
            return Err(TorrentError::bencode(format!(
                "non-canonical byte string length at offset {start}"
            )));
        }
        let length = std::str::from_utf8(length_digits)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| TorrentError::bencode("byte string length is too large"))?;
        self.position += 1;
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| TorrentError::bencode("byte string length overflow"))?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or_else(|| TorrentError::bencode("truncated byte string"))?
            .to_vec();
        self.position = end;
        Ok(bytes)
    }

    fn list(&mut self, depth: usize) -> Result<Vec<Value>> {
        self.position += 1;
        let mut values = Vec::new();
        while self.peek()? != b'e' {
            values.push(self.value(depth + 1)?);
        }
        self.position += 1;
        Ok(values)
    }

    fn dictionary(&mut self, depth: usize) -> Result<BTreeMap<Vec<u8>, Value>> {
        self.position += 1;
        let mut values = BTreeMap::new();
        while self.peek()? != b'e' {
            let key = self.bytes()?;
            let value = self.value(depth + 1)?;
            if values.insert(key, value).is_some() {
                return Err(TorrentError::bencode("dictionary contains a duplicate key"));
            }
        }
        self.position += 1;
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_nested_values() {
        let value = decode(b"d3:bar4:spam3:fooi42e4:listl1:ai-2eee").unwrap();
        assert_eq!(encode(&value), b"d3:bar4:spam3:fooi42e4:listl1:ai-2eee");
        assert_eq!(value.get(b"foo").and_then(Value::as_integer), Some(42));
    }

    #[test]
    fn returns_dictionary_value_byte_range() {
        let data = b"d8:announce1:x4:infod4:name4:testee";
        let range = dictionary_value_range(data, b"info").unwrap();
        assert_eq!(&data[range], b"d4:name4:teste");
    }

    #[test]
    fn rejects_non_canonical_values() {
        assert!(decode(b"i03e").is_err());
        assert!(decode(b"03:abc").is_err());
        assert!(decode(b"d1:ai1e1:ai2ee").is_err());
    }

    #[test]
    fn accepts_unsorted_tracker_dictionaries() {
        let value = decode(b"d5:peers0:8:intervali30ee").unwrap();
        assert_eq!(value.get(b"interval").and_then(Value::as_integer), Some(30));
    }
}
