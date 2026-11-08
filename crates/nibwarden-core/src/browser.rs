//! The encrypted channel of the Bitwarden browser extension's desktop integration
//! (phase2.md §5.8): the extension sends an RSA public key, nibwarden answers with a new
//! AES-256-CBC-HMAC key encrypted to it (RSA-OAEP with SHA-1), and every later message is
//! a type 2 EncString under that key. This is the protocol of Bitwarden's desktop app
//! (`biometric-message-handler.service.ts`, `nativeMessaging.background.ts`), which the
//! extension speaks.

use bitwarden_crypto::{
    BitwardenLegacyKeyBytes, EncString, KeyDecryptable as _, KeyEncryptable as _, PublicKey,
    SpkiPublicKeyBytes, SymmetricCryptoKey, UnsignedSharedKey, generate_random_bytes,
};
use bitwarden_encoding::B64;
use serde_json::{Value, json};

use crate::{Error, error::SdkContext as _};

/// One extension's session key; kept by the daemon per extension (its `appId`).
pub struct BrowserChannel(SymmetricCryptoKey);

impl BrowserChannel {
    /// A new channel for the extension with `public_key` (base64 SPKI DER), and the key
    /// encrypted to it (base64), to send back as `sharedSecret`.
    pub fn new(public_key: &str) -> Result<(Self, String), Error> {
        let der: B64 = public_key
            .parse()
            .map_err(|_| Error::Invalid("the extension's public key is not base64".into()))?;
        let public_key = PublicKey::from_der(&SpkiPublicKeyBytes::from(der.as_bytes()))
            .sdk("read the extension's public key")?;
        let raw: Vec<u8> = generate_random_bytes::<[u8; 64]>().to_vec();
        let key = SymmetricCryptoKey::try_from(&BitwardenLegacyKeyBytes::from(raw))
            .sdk("make a session key")?;
        // The extension decrypts with RSA-OAEP SHA-1, which is what this (deprecated
        // because unsigned) encapsulation does.
        #[allow(deprecated)]
        let shared = UnsignedSharedKey::encapsulate_key_unsigned(&key, &public_key)
            .sdk("encrypt the session key")?;
        let secret = match &shared {
            UnsignedSharedKey::Rsa2048_OaepSha1_B64 { data } => {
                B64::from(data.as_slice()).to_string()
            }
            _ => unreachable!("encapsulate_key_unsigned makes RSA-OAEP SHA-1"),
        };
        Ok((Self(key), secret))
    }

    /// Decrypts a message from the extension: an EncString as JSON, either with
    /// `encryptedString` or with its parts (`iv`, `data`, `mac`).
    pub fn open(&self, message: &Value) -> Result<String, Error> {
        let text = match message.get("encryptedString").and_then(Value::as_str) {
            Some(text) => text.to_owned(),
            None => {
                let part = |name: &str| message.get(name).and_then(Value::as_str);
                match (part("iv"), part("data"), part("mac")) {
                    (Some(iv), Some(data), Some(mac)) => format!("2.{iv}|{data}|{mac}"),
                    _ => return Err(Error::Invalid("not an encrypted message".into())),
                }
            }
        };
        let enc: EncString = text.parse().sdk("read the encrypted message")?;
        enc.decrypt_with_key(&self.0)
            .sdk("decrypt the extension's message")
    }

    /// Encrypts `text` for the extension, as the desktop app serializes an EncString.
    pub fn seal(&self, text: &str) -> Result<Value, Error> {
        let enc = text
            .encrypt_with_key(&self.0)
            .sdk("encrypt a message for the extension")?;
        let EncString::Aes256Cbc_HmacSha256_B64 { iv, mac, data } = &enc else {
            unreachable!("a legacy 64-byte key encrypts with AES-256-CBC-HMAC");
        };
        Ok(json!({
            "encryptedString": enc.to_string(),
            "encryptionType": 2,
            "iv": B64::from(iv.as_slice()).to_string(),
            "data": B64::from(data.as_slice()).to_string(),
            "mac": B64::from(mac.as_slice()).to_string(),
        }))
    }
}

/// The extension's side of the setup, for testing the integration without a browser
/// (`nibwarden-probe browser-extension`).
pub struct ExtensionKeys(bitwarden_crypto::PrivateKey);

impl ExtensionKeys {
    /// A new key pair, and the public key as the extension sends it (base64 SPKI DER).
    pub fn new() -> Result<(Self, String), Error> {
        let private = bitwarden_crypto::PrivateKey::make(
            bitwarden_crypto::PublicKeyEncryptionAlgorithm::RsaOaepSha1,
        );
        let der = private
            .to_public_key()
            .to_der()
            .sdk("encode the public key")?;
        Ok((Self(private), B64::from(der.as_ref()).to_string()))
    }

    /// The channel, from the `sharedSecret` the desktop side answered with.
    pub fn accept(&self, shared_secret: &str) -> Result<BrowserChannel, Error> {
        let shared: UnsignedSharedKey = format!("4.{shared_secret}")
            .parse()
            .sdk("read the shared secret")?;
        #[allow(deprecated)]
        let key = shared
            .decapsulate_key_unsigned(&self.0)
            .sdk("decrypt the shared secret")?;
        Ok(BrowserChannel(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_up_and_round_trips() {
        let (keys, public) = ExtensionKeys::new().unwrap();
        let (channel, secret) = BrowserChannel::new(&public).unwrap();
        let theirs = keys.accept(&secret).unwrap();

        let sealed = channel.seal(r#"{"command":"x"}"#).unwrap();
        assert_eq!(theirs.open(&sealed).unwrap(), r#"{"command":"x"}"#);
        let parts = json!({
            "iv": sealed["iv"], "data": sealed["data"], "mac": sealed["mac"],
        });
        assert_eq!(channel.open(&parts).unwrap(), r#"{"command":"x"}"#);
        assert!(channel.open(&json!({ "data": "x" })).is_err());
    }
}
