use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ids::{HumanAttestationId, ObligationId};
use crate::proof::{DependencyPolicy, DependencySurface};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HumanSignatureAlgorithm {
  Ed25519,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HumanAttestorSpec {
  pub id: String,
  #[serde(rename = "publicKey")]
  pub public_key: String,
  #[serde(default)]
  pub dependencies: DependencyPolicy,
}

impl HumanAttestorSpec {
  pub fn validate(&self) -> Result<(), HumanAttestationError> {
    if self.id.trim().is_empty() {
      return Err(HumanAttestationError::UnknownAttestor);
    }
    self
      .dependencies
      .validate()
      .map_err(|_| HumanAttestationError::InvalidDependencies)?;
    parse_verifying_key(&self.public_key)?;
    Ok(())
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-human-attestor-v1", self)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HumanAttestationRecord {
  pub id: HumanAttestationId,
  #[serde(rename = "attestorId")]
  pub attestor_id: String,
  #[serde(rename = "statementHash")]
  pub statement_hash: String,
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  #[serde(rename = "catalogHash")]
  pub catalog_hash: String,
  pub revision: String,
  #[serde(rename = "issuedAt")]
  pub issued_at: DateTime<Utc>,
  pub algorithm: HumanSignatureAlgorithm,
  #[serde(rename = "publicKey")]
  pub public_key: String,
  pub signature: String,
  pub dependencies: DependencySurface,
}

#[derive(Serialize)]
struct HumanAttestationPayload<'a> {
  domain: &'static str,
  attestor_id: &'a str,
  statement_hash: &'a str,
  obligation_id: &'a ObligationId,
  catalog_hash: &'a str,
  revision: &'a str,
  issued_at: DateTime<Utc>,
  algorithm: HumanSignatureAlgorithm,
  public_key: &'a str,
  dependencies: &'a DependencySurface,
}

pub struct HumanAttestationBinding {
  pub statement_hash: String,
  pub obligation_id: ObligationId,
  pub catalog_hash: String,
  pub revision: String,
  pub issued_at: DateTime<Utc>,
  pub dependencies: DependencySurface,
}

impl HumanAttestationRecord {
  pub fn sign(
    attestor: &HumanAttestorSpec,
    secret_key: &[u8; 32],
    binding: HumanAttestationBinding,
  ) -> Result<Self, HumanAttestationError> {
    let HumanAttestationBinding {
      statement_hash,
      obligation_id,
      catalog_hash,
      revision,
      issued_at,
      dependencies,
    } = binding;
    attestor.validate()?;
    if statement_hash.is_empty() || catalog_hash.is_empty() || revision.trim().is_empty() {
      return Err(HumanAttestationError::MissingBinding);
    }
    let signing_key = SigningKey::from_bytes(secret_key);
    let configured_key = parse_verifying_key(&attestor.public_key)?;
    if signing_key.verifying_key() != configured_key {
      return Err(HumanAttestationError::WrongSigner);
    }
    let mut record = Self {
      id: HumanAttestationId::new(),
      attestor_id: attestor.id.clone(),
      statement_hash,
      obligation_id,
      catalog_hash,
      revision,
      issued_at,
      algorithm: HumanSignatureAlgorithm::Ed25519,
      public_key: attestor.public_key.clone(),
      signature: String::new(),
      dependencies,
    };
    let signature = signing_key.sign(&record.payload()?);
    record.signature = encode_hex(&signature.to_bytes());
    Ok(record)
  }

  pub fn verify(&self, attestor: &HumanAttestorSpec) -> Result<(), HumanAttestationError> {
    attestor.validate()?;
    if self.attestor_id != attestor.id || self.public_key != attestor.public_key {
      return Err(HumanAttestationError::UnknownAttestor);
    }
    let dependencies_match = match (&attestor.dependencies, &self.dependencies) {
      (DependencyPolicy::RepositoryWide, DependencySurface::RepositoryWide) => true,
      (
        DependencyPolicy::Paths { patterns: expected },
        DependencySurface::Paths { patterns, .. },
      ) => expected == patterns,
      _ => false,
    };
    if !dependencies_match {
      return Err(HumanAttestationError::InvalidDependencies);
    }
    if self.statement_hash.is_empty()
      || self.catalog_hash.is_empty()
      || self.revision.trim().is_empty()
      || self.algorithm != HumanSignatureAlgorithm::Ed25519
    {
      return Err(HumanAttestationError::MissingBinding);
    }
    let verifying_key = parse_verifying_key(&self.public_key)?;
    let signature_bytes: [u8; 64] = decode_hex(&self.signature)?
      .try_into()
      .map_err(|_| HumanAttestationError::InvalidSignatureEncoding)?;
    let signature = Signature::from_bytes(&signature_bytes);
    verifying_key
      .verify_strict(&self.payload()?, &signature)
      .map_err(|_| HumanAttestationError::InvalidSignature)
  }

  pub fn record_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-human-attestation-record-v1", self)
  }

  fn payload(&self) -> Result<Vec<u8>, HumanAttestationError> {
    serde_json::to_vec(&HumanAttestationPayload {
      domain: "tenet-human-attestation-v1",
      attestor_id: &self.attestor_id,
      statement_hash: &self.statement_hash,
      obligation_id: &self.obligation_id,
      catalog_hash: &self.catalog_hash,
      revision: &self.revision,
      issued_at: self.issued_at,
      algorithm: self.algorithm,
      public_key: &self.public_key,
      dependencies: &self.dependencies,
    })
    .map_err(|error| HumanAttestationError::Encoding(error.to_string()))
  }
}

fn parse_verifying_key(encoded: &str) -> Result<VerifyingKey, HumanAttestationError> {
  let bytes: [u8; 32] = decode_hex(encoded)?
    .try_into()
    .map_err(|_| HumanAttestationError::InvalidPublicKey)?;
  VerifyingKey::from_bytes(&bytes).map_err(|_| HumanAttestationError::InvalidPublicKey)
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, HumanAttestationError> {
  if !encoded.len().is_multiple_of(2) || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(HumanAttestationError::InvalidSignatureEncoding);
  }
  encoded
    .as_bytes()
    .as_chunks::<2>()
    .0
    .iter()
    .map(|pair| {
      let high = (pair[0] as char)
        .to_digit(16)
        .ok_or(HumanAttestationError::InvalidSignatureEncoding)?;
      let low = (pair[1] as char)
        .to_digit(16)
        .ok_or(HumanAttestationError::InvalidSignatureEncoding)?;
      Ok(((high << 4) | low) as u8)
    })
    .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
  }
  encoded
}

fn fingerprint<T: Serialize>(domain: &str, value: &T) -> Result<String, serde_json::Error> {
  let encoded = serde_json::to_vec(&(domain, value))?;
  let digest = Sha256::digest(encoded);
  Ok(encode_hex(&digest))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HumanAttestationError {
  #[error("human attestor is unknown or does not match the configured identity")]
  UnknownAttestor,
  #[error("human attestor public key is invalid")]
  InvalidPublicKey,
  #[error("human attestation signature encoding is invalid")]
  InvalidSignatureEncoding,
  #[error("human attestation signature is invalid")]
  InvalidSignature,
  #[error("private signing key does not match the configured human attestor")]
  WrongSigner,
  #[error("human attestation is missing a required semantic binding")]
  MissingBinding,
  #[error("human attestation dependency policy is invalid")]
  InvalidDependencies,
  #[error("human attestation payload encoding failed: {0}")]
  Encoding(String),
}

#[cfg(test)]
mod tests {
  use super::*;

  fn attestor(secret: &[u8; 32]) -> HumanAttestorSpec {
    let key = SigningKey::from_bytes(secret).verifying_key();
    HumanAttestorSpec {
      id: "alice".into(),
      public_key: encode_hex(&key.to_bytes()),
      dependencies: Default::default(),
    }
  }

  fn record(secret: &[u8; 32]) -> HumanAttestationRecord {
    HumanAttestationRecord::sign(
      &attestor(secret),
      secret,
      HumanAttestationBinding {
        statement_hash: "statement".into(),
        obligation_id: ObligationId::from("REQ-1/AC-1/VO-1"),
        catalog_hash: "catalog".into(),
        revision: "revision".into(),
        issued_at: Utc::now(),
        dependencies: DependencySurface::RepositoryWide,
      },
    )
    .expect("sign attestation")
  }

  #[test]
  fn valid_explicit_signature_verifies() {
    let secret = [7_u8; 32];
    let record = record(&secret);

    assert!(record.verify(&attestor(&secret)).is_ok());
  }

  #[test]
  fn signature_for_different_revision_is_rejected() {
    let secret = [7_u8; 32];
    let mut record = record(&secret);
    record.revision = "other".into();

    assert_eq!(
      record.verify(&attestor(&secret)).unwrap_err(),
      HumanAttestationError::InvalidSignature
    );
  }

  #[test]
  fn signature_cannot_replay_across_statement_obligation_or_catalog() {
    let secret = [7_u8; 32];
    let valid = record(&secret);
    let mut variants = Vec::new();
    let mut statement = valid.clone();
    statement.statement_hash = "other-statement".into();
    variants.push(statement);
    let mut obligation = valid.clone();
    obligation.obligation_id = ObligationId::from("REQ-1/AC-1/VO-2");
    variants.push(obligation);
    let mut catalog = valid;
    catalog.catalog_hash = "other-catalog".into();
    variants.push(catalog);
    for variant in variants {
      assert_eq!(
        variant.verify(&attestor(&secret)).unwrap_err(),
        HumanAttestationError::InvalidSignature
      );
    }
  }

  #[test]
  fn unsigned_and_unknown_attestor_records_are_rejected() {
    let secret = [7_u8; 32];
    let mut unsigned = record(&secret);
    unsigned.signature.clear();
    assert!(matches!(
      unsigned.verify(&attestor(&secret)),
      Err(HumanAttestationError::InvalidSignatureEncoding)
    ));
    let mut unknown = attestor(&secret);
    unknown.id = "mallory".into();
    assert_eq!(
      record(&secret).verify(&unknown).unwrap_err(),
      HumanAttestationError::UnknownAttestor
    );
  }

  #[test]
  fn wrong_signer_is_rejected_before_issuance() {
    let configured = attestor(&[7_u8; 32]);

    assert_eq!(
      HumanAttestationRecord::sign(
        &configured,
        &[9_u8; 32],
        HumanAttestationBinding {
          statement_hash: "statement".into(),
          obligation_id: ObligationId::from("REQ-1/AC-1/VO-1"),
          catalog_hash: "catalog".into(),
          revision: "revision".into(),
          issued_at: Utc::now(),
          dependencies: DependencySurface::RepositoryWide,
        },
      )
      .unwrap_err(),
      HumanAttestationError::WrongSigner
    );
  }
}
