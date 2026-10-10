//! Who is calling, read from the `authorization` header the way the official emulator reads it
//! (measured by `tools/oracle/auth.py`).
//!
//! `Bearer owner` (what server SDKs send to an emulator) and Google OAuth access tokens
//! (`ya29.…`) are administrators, ignoring case. Any other bearer token must be a JWT, read
//! without verification: any `alg`, any signature, no expiry, audience or subject check, but
//! three URL-safe base64 segments whose first two are JSON objects; anything else is
//! `invalid jwt`. A header that is not a bearer token fails with `UNKNOWN` and no message, an
//! internal error on the official emulator.

use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde_json::{Map, Value};
use tonic::{Status, metadata::MetadataMap};

/// A token's claims, kept for `request.auth` in Security Rules (#9).
pub(crate) type Claims = Map<String, Value>;

#[derive(Debug)]
pub(crate) enum Caller {
    /// No header.
    Anonymous,
    Admin,
    // Read by Security Rules (#9).
    User(#[allow(dead_code)] Claims),
}

impl Caller {
    pub(crate) fn is_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// The caller of a gRPC request (REST requests carry their header here too).
pub(crate) fn caller(metadata: &MetadataMap) -> Result<Caller, Status> {
    from_header(
        metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
    )
}

/// The caller named by an `Authorization` header's value.
pub(crate) fn from_header(value: Option<&str>) -> Result<Caller, Status> {
    // gRPC metadata cannot carry control characters such as a tab; the official emulator
    // ignores a REST header that has one.
    let Some(value) = value.filter(|v| v.bytes().all(|b| (0x20..0x7f).contains(&b))) else {
        return Ok(Caller::Anonymous);
    };
    let token = match value.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("bearer ") => &value[7..],
        _ => return Err(Status::unknown("")),
    };
    if token.eq_ignore_ascii_case("owner")
        || token
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ya29."))
    {
        return Ok(Caller::Admin);
    }
    claims(token)
        .map(Caller::User)
        .ok_or_else(|| Status::invalid_argument("invalid jwt"))
}

fn claims(token: &str) -> Option<Claims> {
    let mut segments = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return None;
    };
    object(header)?;
    decode(signature)?;
    object(claims)
}

fn object(segment: &str) -> Option<Claims> {
    serde_json::from_slice(&decode(segment)?).ok()
}

/// URL-safe base64, with any number of trailing `=` and nonzero trailing bits accepted.
fn decode(segment: &str) -> Option<Vec<u8>> {
    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    ENGINE.decode(segment.trim_end_matches('=')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_claims_of_an_unsigned_token() {
        // What firebase-js-sdk's `createMockUserToken` sends.
        let token = "Bearer eyJhbGciOiJub25lIiwidHlwZSI6IkpXVCJ9.eyJzdWIiOiJhbGljZSIsImZpcmViYXNlIjp7InNpZ25faW5fcHJvdmlkZXIiOiJjdXN0b20ifX0.";
        let Ok(Caller::User(claims)) = from_header(Some(token)) else {
            panic!("a user");
        };
        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims["firebase"]["sign_in_provider"], "custom");
    }

    #[test]
    fn administrators_ignore_case() {
        for value in [
            "Bearer owner",
            "bearer OWNER",
            "BEARER Owner",
            "Bearer ya29.",
            "Bearer YA29.x",
        ] {
            assert!(from_header(Some(value)).unwrap().is_admin(), "{value}");
        }
    }
}
