use signature::Signer as _;
use ssh_key::{HashAlg, PrivateKey, encoding::Encode as _, private::KeypairData};

use crate::proto::{RSA_SHA2_256, RSA_SHA2_512};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("invalid key: {0}")]
    Key(#[from] ssh_key::Error),
    #[error("encoding: {0}")]
    Encoding(#[from] ssh_key::encoding::Error),
    #[error("the stored private key is passphrase-protected")]
    Encrypted,
    #[error("the stored private key does not match the requested public key")]
    Mismatch,
    #[error("signing failed")]
    Signature(#[from] signature::Error),
}

/// Signs `data` with the OpenSSH-format private key `pem`, which must belong to `key_blob`.
/// Returns the encoded signature (`string algorithm || string blob`). For RSA, `flags`
/// picks SHA-256 or SHA-512; without either, the legacy SHA-1 `ssh-rsa` is used, as
/// OpenSSH's agent does.
pub fn sign(pem: &str, key_blob: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, SignError> {
    let key = PrivateKey::from_openssh(pem)?;
    if key.is_encrypted() {
        return Err(SignError::Encrypted);
    }
    if key.public_key().to_bytes()? != key_blob {
        return Err(SignError::Mismatch);
    }
    let signature = match key.key_data() {
        KeypairData::Rsa(rsa) => {
            let hash = if flags & RSA_SHA2_512 != 0 {
                Some(HashAlg::Sha512)
            } else if flags & RSA_SHA2_256 != 0 {
                Some(HashAlg::Sha256)
            } else {
                None
            };
            (rsa, hash).try_sign(data)?
        }
        _ => key.try_sign(data)?,
    };
    Ok(signature.encode_vec()?)
}
