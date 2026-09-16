//! Deterministic admission-grant authorization semantics.
//!
//! An `AdmissionGrant` is a capability bound to one exact proposal/authority
//! pair. Validity requires a keyed HMAC-SHA256 over the canonical grant
//! payload under a secret the candidate producer does not possess. The kernel
//! verifies grants deterministically; no adapter instruction, prompt, or ref
//! participates in this decision.

use sha2::{Digest, Sha256};
use tenet_domain::{
  authority::{ADMISSION_GRANT_SEMANTICS_V1, AdmissionGrant, ProposalId},
  evidence::AuthorityId,
};
use thiserror::Error;

/// Minimum accepted secret entropy in bytes. Shorter secrets are rejected
/// instead of silently weakening the grant.
pub const MIN_SECRET_BYTES: usize = 32;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GrantError {
  #[error("admission grant secret must be at least {MIN_SECRET_BYTES} bytes")]
  WeakSecret,
  #[error("unsupported admission grant version `{0}`")]
  UnsupportedVersion(u32),
  #[error("unsupported admission grant semantics `{0}`")]
  UnsupportedSemantics(String),
  #[error("admission grant mac is not 64 lowercase hex characters")]
  InvalidMacFormat,
  #[error("admission grant is not bound to the exact admitted proposal")]
  ProposalMismatch,
  #[error("admission grant is not bound to the exact admitted authority")]
  AuthorityMismatch,
  #[error("admission grant mac does not verify under the trusted secret")]
  MacMismatch,
}

/// Payload whose canonical JSON is the MAC message; identical to the grant
/// without the `mac` field.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantPayload<'a> {
  schema_version: u32,
  semantics: &'a str,
  proposal: &'a ProposalId,
  authority: &'a AuthorityId,
}

fn grant_message(grant: &AdmissionGrant) -> Result<Vec<u8>, serde_json::Error> {
  serde_json::to_vec(&GrantPayload {
    schema_version: grant.schema_version,
    semantics: &grant.semantics,
    proposal: &grant.proposal,
    authority: &grant.authority,
  })
}

/// RFC 2104 HMAC-SHA256.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
  const BLOCK: usize = 64;
  let mut key_block = [0_u8; BLOCK];
  if key.len() > BLOCK {
    key_block[..32].copy_from_slice(&Sha256::digest(key));
  } else {
    key_block[..key.len()].copy_from_slice(key);
  }
  let mut ipad_hasher = Sha256::new();
  let mut opad_hasher = Sha256::new();
  for byte in key_block {
    ipad_hasher.update([byte ^ 0x36]);
    opad_hasher.update([byte ^ 0x5c]);
  }
  ipad_hasher.update(message);
  let inner = ipad_hasher.finalize();
  opad_hasher.update(inner);
  opad_hasher.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
  if left.len() != right.len() {
    return false;
  }
  let difference = left
    .iter()
    .zip(right)
    .fold(0_u8, |acc, (left, right)| acc | (left ^ right));
  difference == 0
}

fn hex_lower(bytes: &[u8]) -> String {
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    encoded.push(HEX[(byte >> 4) as usize] as char);
    encoded.push(HEX[(byte & 0x0f) as usize] as char);
  }
  encoded
}

fn decode_mac(mac: &str) -> Option<Vec<u8>> {
  if mac.len() != 64
    || !mac
      .as_bytes()
      .iter()
      .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
  {
    return None;
  }
  let bytes = mac.as_bytes();
  let mut decoded = Vec::with_capacity(32);
  for pair in bytes.chunks(2) {
    let high = (pair[0] as char).to_digit(16)?;
    let low = (pair[1] as char).to_digit(16)?;
    decoded.push((high << 4 | low) as u8);
  }
  Some(decoded)
}

fn validate_secret(secret: &[u8]) -> Result<(), GrantError> {
  if secret.len() < MIN_SECRET_BYTES {
    return Err(GrantError::WeakSecret);
  }
  Ok(())
}

/// True when the grant's structural binding is exact for the admitted chain:
/// supported version/semantics and identical proposal/authority identities.
/// This check requires no secret and therefore runs on every load path.
pub fn validate_grant_binding(
  grant: &AdmissionGrant,
  proposal: &ProposalId,
  authority: &AuthorityId,
) -> Result<(), GrantError> {
  if grant.schema_version != 1 {
    return Err(GrantError::UnsupportedVersion(grant.schema_version));
  }
  if grant.semantics != ADMISSION_GRANT_SEMANTICS_V1 {
    return Err(GrantError::UnsupportedSemantics(grant.semantics.clone()));
  }
  if &grant.proposal != proposal {
    return Err(GrantError::ProposalMismatch);
  }
  if &grant.authority != authority {
    return Err(GrantError::AuthorityMismatch);
  }
  if decode_mac(&grant.mac).is_none() {
    return Err(GrantError::InvalidMacFormat);
  }
  Ok(())
}

/// Verify the grant mac under the trusted secret. The grant must also bind
/// exactly to the supplied proposal/authority identities.
pub fn verify_grant(
  secret: &[u8],
  grant: &AdmissionGrant,
  proposal: &ProposalId,
  authority: &AuthorityId,
) -> Result<(), GrantError> {
  validate_secret(secret)?;
  validate_grant_binding(grant, proposal, authority)?;
  let message = grant_message(grant).map_err(|_| GrantError::InvalidMacFormat)?;
  let expected = hmac_sha256(secret, &message);
  let Some(mac) = decode_mac(&grant.mac) else {
    return Err(GrantError::InvalidMacFormat);
  };
  if !constant_time_eq(&expected, &mac) {
    return Err(GrantError::MacMismatch);
  }
  Ok(())
}

/// Mint a grant for one exact proposal/authority pair under the trusted
/// secret. Only a process possessing the secret can produce a valid grant.
pub fn mint_grant(
  secret: &[u8],
  proposal: &ProposalId,
  authority: &AuthorityId,
) -> Result<AdmissionGrant, GrantError> {
  validate_secret(secret)?;
  let grant = AdmissionGrant {
    schema_version: 1,
    semantics: ADMISSION_GRANT_SEMANTICS_V1.into(),
    proposal: proposal.clone(),
    authority: authority.clone(),
    mac: String::new(),
  };
  let message = grant_message(&grant).map_err(|_| GrantError::InvalidMacFormat)?;
  let mac = hmac_sha256(secret, &message);
  Ok(AdmissionGrant {
    mac: hex_lower(&mac),
    ..grant
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::evidence::ContentObjectId;

  fn secret() -> Vec<u8> {
    b"s".repeat(48).to_vec()
  }

  fn ids() -> (ProposalId, AuthorityId) {
    (
      ProposalId(ContentObjectId("sha256:aaa".into())),
      AuthorityId(ContentObjectId("sha256:bbb".into())),
    )
  }

  #[test]
  fn hmac_matches_rfc_4231_vectors() {
    // RFC 4231 HMAC-SHA-256 test cases 1 and 2.
    let key = [0x0b; 20];
    let mac = hmac_sha256(&key, b"Hi There");
    assert_eq!(
      hex_lower(&mac),
      "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
      hex_lower(&mac),
      "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // Long-key path (key longer than the block size) still produces a digest.
    let key = b"k".repeat(200);
    assert_eq!(hmac_sha256(&key, b"abc").len(), 32);
  }

  #[test]
  fn grant_mints_verifies_and_rejects_forgery() {
    let secret = secret();
    let (proposal, authority) = ids();
    let grant = mint_grant(&secret, &proposal, &authority).unwrap();
    assert_eq!(grant.mac.len(), 64);
    verify_grant(&secret, &grant, &proposal, &authority).unwrap();

    // Another secret cannot verify.
    let other = b"t".repeat(48);
    assert_eq!(
      verify_grant(&other, &grant, &proposal, &authority),
      Err(GrantError::MacMismatch)
    );

    // Tampered mac.
    let mut tampered = grant.clone();
    tampered.mac.make_ascii_uppercase();
    assert_eq!(
      verify_grant(&secret, &tampered, &proposal, &authority),
      Err(GrantError::InvalidMacFormat)
    );
    let mut tampered = grant.clone();
    tampered.mac.remove(0);
    tampered.mac.push('0');
    assert_eq!(
      verify_grant(&secret, &tampered, &proposal, &authority),
      Err(GrantError::MacMismatch)
    );

    // Tampered binding.
    let mut tampered = grant.clone();
    tampered.proposal = ProposalId(ContentObjectId("sha256:aaa1".into()));
    assert_eq!(
      verify_grant(&secret, &tampered, &proposal, &authority),
      Err(GrantError::ProposalMismatch)
    );
    let mut tampered = grant.clone();
    tampered.authority = AuthorityId(ContentObjectId("sha256:ccc".into()));
    assert_eq!(
      verify_grant(&secret, &tampered, &proposal, &authority),
      Err(GrantError::AuthorityMismatch)
    );

    // Unknown semantics and version fail closed.
    let mut tampered = grant.clone();
    tampered.semantics = "tenet:admission-grant:v2".into();
    assert_eq!(
      validate_grant_binding(&tampered, &proposal, &authority),
      Err(GrantError::UnsupportedSemantics(tampered.semantics.clone()))
    );
    let mut tampered = grant;
    tampered.schema_version = 2;
    assert_eq!(
      validate_grant_binding(&tampered, &proposal, &authority),
      Err(GrantError::UnsupportedVersion(2))
    );

    // Weak secrets are rejected rather than silently accepted.
    assert_eq!(
      mint_grant(b"short", &proposal, &authority),
      Err(GrantError::WeakSecret)
    );
  }

  #[test]
  fn grant_is_bound_to_exact_ids_not_prefixes() {
    let secret = secret();
    let (proposal, authority) = ids();
    let grant = mint_grant(&secret, &proposal, &authority).unwrap();
    let near = AuthorityId(ContentObjectId("sha256:bbb0".into()));
    assert_eq!(
      validate_grant_binding(&grant, &proposal, &near),
      Err(GrantError::AuthorityMismatch)
    );
  }
}
