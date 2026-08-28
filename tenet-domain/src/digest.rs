use serde::Serialize;
use sha2::{Digest, Sha256};

pub fn bytes_digest(bytes: &[u8]) -> String {
  let digest = Sha256::digest(bytes);
  let mut encoded = String::with_capacity(7 + digest.len() * 2);
  encoded.push_str("sha256:");
  const HEX: &[u8; 16] = b"0123456789abcdef";
  for byte in digest {
    encoded.push(HEX[(byte >> 4) as usize] as char);
    encoded.push(HEX[(byte & 0x0f) as usize] as char);
  }
  encoded
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
  serde_json::to_vec(value).map(|bytes| bytes_digest(&bytes))
}
