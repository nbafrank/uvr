use sha2::{Digest, Sha256};
use md5::Md5;

use crate::error::{Result, UvrError};

/// Compute `sha256:<hex>` for the given bytes.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Compute `md5:<hex>` for the given bytes.
pub fn md5_hex(data: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(data);
    format!("md5:{}", hex::encode(h.finalize()))
}

/// Verify a checksum string (`"sha256:<hex>"` or `"md5:<hex>"`) against data.
pub fn verify(expected: &str, data: &[u8], package: &str) -> Result<()> {
    let actual = if expected.starts_with("sha256:") {
        sha256_hex(data)
    } else if expected.starts_with("md5:") {
        md5_hex(data)
    } else {
        return Err(UvrError::Other(format!(
            "Unknown checksum algorithm in '{expected}'"
        )));
    };

    if actual != expected {
        return Err(UvrError::ChecksumMismatch {
            package: package.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_roundtrip() {
        let data = b"hello world";
        let checksum = sha256_hex(data);
        assert!(checksum.starts_with("sha256:"));
        verify(&checksum, data, "test").unwrap();
    }

    #[test]
    fn md5_roundtrip() {
        let data = b"hello world";
        let checksum = md5_hex(data);
        assert!(checksum.starts_with("md5:"));
        verify(&checksum, data, "test").unwrap();
    }

    #[test]
    fn mismatch_errors() {
        let data = b"hello";
        let checksum = sha256_hex(data);
        let result = verify(&checksum, b"world", "mypkg");
        assert!(result.is_err());
    }
}
