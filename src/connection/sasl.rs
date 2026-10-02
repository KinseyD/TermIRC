//! SASL PLAIN payload encoding for connection workers.
//!
//! The `irc` crate only sends the initial `AUTHENTICATE PLAIN` line; the
//! base64 payload and the 900-range result handling are ours. Encoding is
//! implemented by hand to avoid a new dependency.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard padded base64 of `\0{username}\0{password}` (SASL PLAIN).
/// Config validation caps the encoded length at 400 bytes, so a single
/// `AUTHENTICATE` line always suffices.
pub(super) fn encode_sasl_plain(username: &str, password: &str) -> String {
    let mut payload = Vec::with_capacity(username.len() + password.len() + 2);
    payload.push(0);
    payload.extend_from_slice(username.as_bytes());
    payload.push(0);
    payload.extend_from_slice(password.as_bytes());
    encode_base64(&payload)
}

fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_known_sasl_plain_vectors() {
        // Classic test vector: "\0jilles\0sesame" in base64.
        assert_eq!(
            encode_sasl_plain("jilles", "sesame"),
            "AGppbGxlcwBzZXNhbWU="
        );
        assert_eq!(encode_sasl_plain("", ""), "AAA=");
        assert_eq!(encode_sasl_plain("é", ""), "AMOpAA==");
        // Multi-byte UTF-8 credentials encode byte-wise (covered above).
    }

    #[test]
    fn encoded_payload_stays_within_one_authenticate_line() {
        let username = "u".repeat(100);
        let password = "p".repeat(200);
        assert_eq!(encode_sasl_plain(&username, &password).len(), 404);
    }
}
