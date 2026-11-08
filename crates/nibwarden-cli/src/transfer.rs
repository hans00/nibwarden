//! `nibwarden export` and `nibwarden import` (phase2.md P2-M13).

use std::{io::Write as _, path::PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;
use nibwarden_cli::call_vault;
use nibwarden_ipc::{
    Client, ExportFormatData, ExportResult, ImportResult, Request, import_needs_password,
};
use nibwarden_platform::fs::{create_private_file, restrict_file};

use crate::read_secret;

/// Mirrors the daemon's limit (nibwarden-core `MAX_IMPORT_FILE`), to fail before reading
/// a huge file.
const MAX_FILE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, ValueEnum)]
pub enum Format {
    /// Bitwarden JSON, not encrypted.
    Json,
    /// Bitwarden CSV, not encrypted (logins and secure notes only).
    Csv,
    /// Bitwarden JSON protected by a password you choose.
    EncryptedJson,
}

pub async fn export(
    client: &mut Client,
    format: Format,
    output: PathBuf,
    force: bool,
    password_stdin: bool,
) -> Result<()> {
    if output.exists() && !force {
        bail!(
            "{} already exists; pass --force to replace it",
            output.display()
        );
    }
    let master_password = read_secret("Master password: ", password_stdin)?;
    let format = match format {
        Format::Json => ExportFormatData::Json,
        Format::Csv => ExportFormatData::Csv,
        Format::EncryptedJson => {
            let password = read_secret("Password for the file: ", password_stdin)?;
            if !password_stdin {
                let again = read_secret("Password for the file again: ", false)?;
                if *again != *password {
                    bail!("the passwords do not match");
                }
            }
            if password.is_empty() {
                bail!("the file password is empty");
            }
            ExportFormatData::EncryptedJson { password }
        }
    };
    let result: ExportResult = call_vault(
        client,
        Request::VaultExport {
            format,
            master_password,
        },
    )
    .await?;
    // Private from the moment it exists: an unencrypted export holds every secret.
    let mut file = create_private_file(&output)
        .with_context(|| format!("cannot write {}", output.display()))?;
    // A replaced file keeps its old permissions otherwise.
    restrict_file(&output).with_context(|| format!("cannot restrict {}", output.display()))?;
    file.write_all(result.data.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("cannot write {}", output.display()))?;
    eprintln!("Exported {} items to {}.", result.items, output.display());
    Ok(())
}

pub async fn import(client: &mut Client, path: PathBuf, password_stdin: bool) -> Result<()> {
    let size = std::fs::metadata(&path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    if size > MAX_FILE {
        bail!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_FILE / 1024 / 1024
        );
    }
    let data = zeroize::Zeroizing::new(
        std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?,
    );
    let password = if import_needs_password(&data) {
        Some(read_secret("Password of the file: ", password_stdin)?)
    } else {
        None
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let result: ImportResult = call_vault(
        client,
        Request::VaultImport {
            name,
            data,
            password,
        },
    )
    .await?;
    eprintln!(
        "Imported {} items ({} new folders).",
        result.items, result.folders
    );
    Ok(())
}
