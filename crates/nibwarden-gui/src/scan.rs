//! Filling the edit form from outside the window: a one-time code's QR code from the
//! screen or an image file, and an SSH private key from a file (chosen, or dropped on the
//! window).

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

/// Key files are a few kilobytes; anything much larger is not one.
const MAX_KEY_FILE: u64 = 64 * 1024;
/// Screenshots and photos of a QR code.
const MAX_IMAGE_FILE: u64 = 64 * 1024 * 1024;

/// Why nothing was filled in. `Cancelled` is not shown.
#[derive(Debug)]
pub enum Problem {
    Cancelled,
    NoQrCode,
    NotAKey,
    Unreadable(String),
    NoScreenshot(String),
}

/// The `otpauth://` (or `steam://`) address in the first authenticator QR code of an
/// encoded image.
pub fn authenticator_in_image(bytes: &[u8]) -> Result<Zeroizing<String>, Problem> {
    let image = image::load_from_memory(bytes)
        .map_err(|e| Problem::Unreadable(e.to_string()))?
        .into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(image);
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok())
        .map(|(_, content)| Zeroizing::new(content))
        .find(|content| is_authenticator(content))
        .ok_or(Problem::NoQrCode)
}

fn is_authenticator(content: &str) -> bool {
    let lower = content.get(..10).unwrap_or(content).to_ascii_lowercase();
    lower.starts_with("otpauth://") || lower.starts_with("steam://")
}

/// A private key file's content: OpenSSH, PKCS#8 or PEM. Whether it parses is for the
/// daemon to say when the item is saved.
pub fn key_in_file(path: &Path) -> Result<Zeroizing<String>, Problem> {
    let bytes = read_limited(path, MAX_KEY_FILE)?;
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| Problem::NotAKey)?;
    let text = Zeroizing::new(text);
    if !text.contains("PRIVATE KEY-----") {
        return Err(Problem::NotAKey);
    }
    Ok(Zeroizing::new(text.trim().to_owned()))
}

/// The QR code in an image file.
pub fn authenticator_in_file(path: &Path) -> Result<Zeroizing<String>, Problem> {
    authenticator_in_image(&read_limited(path, MAX_IMAGE_FILE)?)
}

fn read_limited(path: &Path, limit: u64) -> Result<Zeroizing<Vec<u8>>, Problem> {
    let unreadable = |e: std::io::Error| Problem::Unreadable(e.to_string());
    if std::fs::metadata(path).map_err(unreadable)?.len() > limit {
        return Err(Problem::Unreadable("the file is too large".to_owned()));
    }
    std::fs::read(path).map(Zeroizing::new).map_err(unreadable)
}

/// Asks for a file. `images`: only images (for a QR code); otherwise anything (key files
/// often have no extension).
pub async fn choose_file(images: bool) -> Result<PathBuf, Problem> {
    let mut dialog = rfd::AsyncFileDialog::new();
    if images {
        dialog = dialog.add_filter("Images", &["png", "jpg", "jpeg"]);
    } else if let Some(ssh) = std::env::var_os("HOME").map(|home| Path::new(&home).join(".ssh")) {
        dialog = dialog.set_directory(ssh);
    }
    dialog
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
        .ok_or(Problem::Cancelled)
}

/// Whether the screen can be scanned here.
pub const CAN_SCAN_SCREEN: bool = cfg!(target_os = "linux");

/// The QR code on the screen, from a screenshot the desktop portal takes (the desktop may
/// ask the user first).
#[cfg(target_os = "linux")]
pub async fn authenticator_on_screen() -> Result<Zeroizing<String>, Problem> {
    use ashpd::desktop::{ResponseError, screenshot::Screenshot};

    let response = Screenshot::request()
        .interactive(false)
        .modal(false)
        .send()
        .await
        .and_then(|request| request.response());
    let screenshot = match response {
        Ok(screenshot) => screenshot,
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Err(Problem::Cancelled),
        Err(e) => return Err(Problem::NoScreenshot(e.to_string())),
    };
    let path = screenshot
        .uri()
        .to_file_path()
        .map_err(|()| Problem::NoScreenshot(format!("not a file: {}", screenshot.uri())))?;
    let bytes = read_limited(&path, MAX_IMAGE_FILE);
    // The portal saved the whole screen for this; do not leave it behind.
    let _ = std::fs::remove_file(&path);
    let bytes = bytes?;
    tokio::task::spawn_blocking(move || authenticator_in_image(&bytes))
        .await
        .map_err(|e| Problem::Unreadable(e.to_string()))?
}

#[cfg(not(target_os = "linux"))]
pub async fn authenticator_on_screen() -> Result<Zeroizing<String>, Problem> {
    Err(Problem::NoScreenshot("not supported here".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_authenticator_codes() {
        assert!(is_authenticator("otpauth://totp/x?secret=ABC"));
        assert!(is_authenticator("OTPAUTH://TOTP/x"));
        assert!(is_authenticator("steam://ABC"));
        assert!(!is_authenticator("https://example.com"));
        assert!(!is_authenticator("otp"));
    }

    #[test]
    fn key_files() {
        let dir = std::env::temp_dir().join(format!("nibwarden-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("id_test");
        std::fs::write(
            &key,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n\n",
        )
        .unwrap();
        assert!(key_in_file(&key).unwrap().ends_with("KEY-----"));
        let public = dir.join("id_test.pub");
        std::fs::write(&public, "ssh-ed25519 AAAA test").unwrap();
        assert!(matches!(key_in_file(&public), Err(Problem::NotAKey)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_an_authenticator_code() {
        let uri = "otpauth://totp/Example:alice?secret=JBSWY3DPEHPK3PXP&issuer=Example";
        let code = qrcode::QrCode::new(uri).unwrap();
        let image = code
            .render::<image::Luma<u8>>()
            .min_dimensions(300, 300)
            .build();
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(authenticator_in_image(&png).unwrap().as_str(), uri);

        // A QR code that is not an authenticator's is not taken.
        let other = qrcode::QrCode::new("https://example.com").unwrap();
        let image = other.render::<image::Luma<u8>>().build();
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(matches!(
            authenticator_in_image(&png),
            Err(Problem::NoQrCode)
        ));
    }

    #[test]
    fn no_code_in_a_blank_image() {
        let mut png = Vec::new();
        image::GrayImage::from_pixel(64, 64, image::Luma([255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(matches!(
            authenticator_in_image(&png),
            Err(Problem::NoQrCode)
        ));
    }
}
