//! Password and passphrase generation. Needs no account or unlocked vault.

use bitwarden_generators::{PassphraseGeneratorRequest, PasswordGeneratorRequest};
use bitwarden_pm::PasswordManagerClient;
use zeroize::Zeroizing;

use crate::{Error, error::SdkContext as _};

#[derive(Debug, Clone)]
pub struct PasswordOptions {
    pub length: u8,
    pub uppercase: bool,
    pub lowercase: bool,
    pub numbers: bool,
    pub special: bool,
    pub avoid_ambiguous: bool,
}

#[derive(Debug, Clone)]
pub struct PassphraseOptions {
    pub words: u8,
    pub separator: String,
    pub capitalize: bool,
    pub include_number: bool,
}

pub fn generate_password(options: &PasswordOptions) -> Result<Zeroizing<String>, Error> {
    let request = PasswordGeneratorRequest {
        length: options.length,
        uppercase: options.uppercase,
        lowercase: options.lowercase,
        numbers: options.numbers,
        special: options.special,
        avoid_ambiguous: options.avoid_ambiguous,
        ..Default::default()
    };
    PasswordManagerClient::new(None)
        .generator()
        .password(request)
        .map(Zeroizing::new)
        .map_err(|e| Error::Invalid(e.to_string()))
}

pub fn generate_passphrase(options: &PassphraseOptions) -> Result<Zeroizing<String>, Error> {
    let request = PassphraseGeneratorRequest {
        num_words: options.words,
        word_separator: options.separator.clone(),
        capitalize: options.capitalize,
        include_number: options.include_number,
    };
    PasswordManagerClient::new(None)
        .generator()
        .passphrase(request)
        .map(Zeroizing::new)
        .sdk("generate passphrase")
}
