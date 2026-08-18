// Hex is how every secret in Pandora crosses a boundary — a capability in a URL path, an API token
// in `api.pandora`, a keyvault digest — and four call sites had each grown their own encoder around
// a per-byte `format!`, which allocates and formats a String for every one of a token's 32 bytes.

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

pub fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        output.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

// A 256-bit capability, hex encoded. The error is handed back rather than absorbed because the
// callers disagree about what a dead CSPRNG means: a batch drops its output page, an upload
// transfer fails, `/gentoken` reports the entropy source by name.
pub fn random_hex_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)?;
    Ok(hex_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex_bytes(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex_bytes(&[]), "");
    }

    // Every consumer of a capability checks it is 64 hex digits before using it, so a token that is
    // anything else is a token nothing will accept.
    #[test]
    fn a_token_is_sixty_four_hex_digits_and_does_not_repeat() {
        let token = random_hex_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(token, random_hex_token().unwrap());
    }
}
