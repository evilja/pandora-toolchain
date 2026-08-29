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

// The identity an HLS release names its playlists and chunks after. A random v4 UUID rather than a
// counter so nothing about one output's filenames tells a viewer what the next release's are called;
// it is an identifier, not the capability — `random_hex_token` still guards the directory.
pub fn random_uuid_v4() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex_bytes(&bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
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

    // Every HLS filename is built from this, and the serving side accepts only the canonical form,
    // so a UUID that is laid out any other way is a UUID nothing will serve.
    #[test]
    fn a_uuid_is_canonical_lowercase_version_four() {
        let uuid = random_uuid_v4().unwrap();
        assert_eq!(uuid.len(), 36);
        let groups: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            groups.iter().map(|group| group.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(groups.iter().all(|group| group
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))));
        assert!(groups[2].starts_with('4'));
        assert!(["8", "9", "a", "b"].contains(&&groups[3][0..1]));
        assert_ne!(uuid, random_uuid_v4().unwrap());
    }
}
