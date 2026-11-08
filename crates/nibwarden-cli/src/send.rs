//! `nibwarden send` (phase2.md P2-M12): share text or a file through a Send link.

use std::{
    io::{IsTerminal as _, Read as _},
    path::PathBuf,
};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use nibwarden_cli::call_vault;
use nibwarden_ipc::{
    Client, NewSendData, Request, SavedResult, SendBodyData, SendEntry, SendListResult,
    SendSecretResult,
};
use nibwarden_platform::terminal;
use zeroize::Zeroizing;

/// Mirrors the daemon's limit (nibwarden-core `MAX_SEND_FILE`), to fail before reading
/// a huge file.
const MAX_FILE: u64 = 10 * 1024 * 1024;

#[derive(Subcommand)]
pub enum SendCommand {
    /// List your Sends (names, kinds, dates; never the content or link).
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a Send and print its link. The text is read from stdin.
    Create {
        name: String,
        /// Send this file instead of text (at most 10 MiB).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Hide the text on the receiving page until the recipient clicks to show it.
        #[arg(long, conflicts_with = "file")]
        hidden: bool,
        /// Days until the server deletes it (1 to 31).
        #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u32).range(1..=31))]
        days: u32,
        /// Stop opening after this many accesses.
        #[arg(long)]
        max_access: Option<u32>,
        /// Ask (in the terminal) for a password the recipient must enter.
        #[arg(long)]
        password: bool,
        /// Hide your email address from the recipient.
        #[arg(long)]
        hide_email: bool,
        /// Private notes about this Send.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Print a Send's link, or with --text its text.
    Get {
        /// Send name or id.
        send: String,
        #[arg(long)]
        text: bool,
    },
    /// Delete a Send: its link stops working.
    Delete {
        /// Send name or id.
        send: String,
    },
}

pub async fn run(client: &mut Client, command: SendCommand) -> Result<()> {
    match command {
        SendCommand::List { json } => {
            let result: SendListResult = call_vault(client, Request::SendList).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result.sends)?);
            } else {
                print_sends(&result.sends);
            }
        }
        SendCommand::Create {
            name,
            file,
            hidden,
            days,
            max_access,
            password,
            hide_email,
            notes,
        } => {
            let body = match file {
                Some(path) => file_body(&path)?,
                None => SendBodyData::Text {
                    text: read_text()?,
                    hidden,
                },
            };
            let password = if password {
                let password = terminal::read_password("Password for the recipient: ")
                    .context("no terminal to read the password from")?;
                Some(password)
            } else {
                None
            };
            let send = NewSendData {
                name,
                notes,
                body,
                deletion_days: days,
                max_access_count: max_access,
                password,
                hide_email,
            };
            let saved: SavedResult = call_vault(client, Request::SendCreate { send }).await?;
            let secret: SendSecretResult =
                call_vault(client, Request::SendGet { id: saved.id }).await?;
            println!("{}", secret.link.as_str());
        }
        SendCommand::Get { send, text } => {
            let id = find(client, &send).await?.id;
            let secret: SendSecretResult = call_vault(client, Request::SendGet { id }).await?;
            if !text {
                println!("{}", secret.link.as_str());
            } else if let Some(text) = &secret.text {
                println!("{}", text.as_str());
            } else {
                bail!("{send:?} is a file Send; open its link to download the file");
            }
        }
        SendCommand::Delete { send } => {
            let found = find(client, &send).await?;
            call_vault::<()>(client, Request::SendDelete { id: found.id }).await?;
            eprintln!("Deleted {:?}.", found.name);
        }
    }
    Ok(())
}

fn print_sends(sends: &[SendEntry]) {
    if sends.is_empty() {
        eprintln!("No Sends.");
        return;
    }
    for send in sends {
        let kind = match &send.file_name {
            Some(file) => format!("file {file}"),
            None => "text".to_owned(),
        };
        let date = send.deletion_date.get(..10).unwrap_or(&send.deletion_date);
        let mut line = format!("{}  [{kind}]  deleted {date}", send.name);
        if send.disabled {
            line.push_str("  (disabled)");
        }
        println!("{line}  {}", send.id);
    }
}

/// The Send with this id, or the only one with this name (ignoring case).
async fn find(client: &mut Client, wanted: &str) -> Result<SendEntry> {
    let result: SendListResult = call_vault(client, Request::SendList).await?;
    let lower = wanted.to_lowercase();
    let mut matches: Vec<_> = result
        .sends
        .into_iter()
        .filter(|s| s.id == wanted || s.name.to_lowercase() == lower)
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!("no Send named {wanted:?}"),
        n => bail!("{n} Sends are named {wanted:?}; pass the id instead"),
    }
}

/// The text to send: stdin, up to its end (Ctrl+D in a terminal).
fn read_text() -> Result<Zeroizing<String>> {
    if std::io::stdin().is_terminal() {
        eprintln!("Type the text to send, then press Ctrl+D:");
    }
    let mut text = Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .read_to_string(&mut text)
        .context("cannot read the text from stdin")?;
    // A pipe usually ends with a newline that is not part of the text.
    let len = text.trim_end_matches(['\r', '\n']).len();
    text.truncate(len);
    if text.is_empty() {
        bail!("no text to send; pipe it in, or use --file");
    }
    Ok(text)
}

fn file_body(path: &std::path::Path) -> Result<SendBodyData> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    if size > MAX_FILE {
        bail!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_FILE / 1024 / 1024
        );
    }
    let data = Zeroizing::new(
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    );
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    Ok(SendBodyData::File { name, data })
}
