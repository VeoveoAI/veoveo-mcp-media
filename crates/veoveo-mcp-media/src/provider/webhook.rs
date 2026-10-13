//! The provider's webhook signature: HMAC-SHA256 over `{webhook-id}.{webhook-timestamp}.{body}`
//! with the account's webhook secret, whose `whsec_` prefix is dropped and whose rest is
//! the raw key. The `webhook-signature` header carries one or more space-separated
//! `v3,<hex>` signatures.
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The header that names the event.
pub const ID_HEADER: &str = "webhook-id";
/// The header that carries the signing time in Unix seconds.
pub const TIMESTAMP_HEADER: &str = "webhook-timestamp";
/// The header that carries the signatures.
pub const SIGNATURE_HEADER: &str = "webhook-signature";

/// Why a webhook's signature was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("the signing time is no Unix timestamp")]
    Timestamp,
    #[error("the signing time is {0} seconds from now")]
    Skew(u64),
    #[error("no signature matches")]
    Mismatch,
}

fn mac(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> HmacSha256 {
    let key = secret.strip_prefix("whsec_").unwrap_or(secret);
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// Check a webhook against `secret`, refusing a signing time more than `tolerance_secs`
/// from `now` in either direction.
pub fn verify(
    secret: &str,
    id: &str,
    timestamp: &str,
    body: &[u8],
    signatures: &str,
    now: i64,
    tolerance_secs: u64,
) -> Result<(), SignatureError> {
    let signed: i64 = timestamp.parse().map_err(|_| SignatureError::Timestamp)?;
    let skew = now.abs_diff(signed);
    if skew > tolerance_secs {
        return Err(SignatureError::Skew(skew));
    }
    for part in signatures.split_whitespace() {
        let Some(hex_signature) = part.strip_prefix("v3,") else {
            continue;
        };
        let Ok(signature) = hex::decode(hex_signature) else {
            continue;
        };
        if mac(secret, id, timestamp, body)
            .verify_slice(&signature)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(SignatureError::Mismatch)
}

/// The `v3,<hex>` signature of a webhook, as the provider computes it. The fake provider
/// signs with it.
pub fn sign(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> String {
    format!(
        "v3,{}",
        hex::encode(mac(secret, id, timestamp, body).finalize().into_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test-secret-key";
    const NOW: i64 = 1_700_000_000;

    #[test]
    fn a_signed_webhook_verifies_within_the_tolerance() {
        let body = br#"{"id":"abc","status":"completed"}"#;
        let signature = sign(SECRET, "msg_1", "1700000010", body);
        assert_eq!(
            verify(SECRET, "msg_1", "1700000010", body, &signature, NOW, 300),
            Ok(())
        );
        // Another signature beside the valid one does not matter.
        let both = format!("v3,00ff {signature}");
        assert_eq!(
            verify(SECRET, "msg_1", "1700000010", body, &both, NOW, 300),
            Ok(())
        );
    }

    #[test]
    fn tampering_a_wrong_scheme_and_a_stale_time_are_refused() {
        let signature = sign(SECRET, "msg_1", "1700000000", b"original");
        assert_eq!(
            verify(
                SECRET,
                "msg_1",
                "1700000000",
                b"tampered",
                &signature,
                NOW,
                300
            ),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(
                SECRET,
                "msg_2",
                "1700000000",
                b"original",
                &signature,
                NOW,
                300
            ),
            Err(SignatureError::Mismatch)
        );
        let v1 = signature.replace("v3,", "v1,");
        assert_eq!(
            verify(SECRET, "msg_1", "1700000000", b"original", &v1, NOW, 300),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(
                SECRET,
                "msg_1",
                "1700000000",
                b"original",
                &signature,
                NOW + 301,
                300
            ),
            Err(SignatureError::Skew(301))
        );
        assert_eq!(
            verify(SECRET, "msg_1", "soon", b"original", &signature, NOW, 300),
            Err(SignatureError::Timestamp)
        );
    }

    #[test]
    fn extreme_times_refuse_without_overflow() {
        for timestamp in [i64::MIN, i64::MAX] {
            let timestamp = timestamp.to_string();
            let signature = sign(SECRET, "event", &timestamp, b"{}");
            assert!(matches!(
                verify(SECRET, "event", &timestamp, b"{}", &signature, NOW, 300),
                Err(SignatureError::Skew(_))
            ));
        }
    }
}
