//! Project tasks, run as `cargo xtask <task>` (the alias is in `.cargo/config.toml`):
//!
//! - `check`: formatting, clippy and the unit tests; with `--target`, also whether the
//!   workspace compiles for other platforms.
//! - `install`: puts the built programs and their files under a prefix, for distribution
//!   recipes (PKGBUILD, Nix) and for installing from source.
//! - `dist`: the release artifacts in `target/dist`: .deb, .rpm, a binary tarball, the
//!   source tarball, the PKGBUILD that builds from it, and their checksums.
//!
//! Packages are written directly (ar, tar, rpm), so no distribution tools are needed.

use std::{
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::{Parser, Subcommand};
use flate2::{Compression, GzBuilder};
use sha2::{Digest as _, Sha256};

const NAME: &str = "nibwarden";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const HOMEPAGE: &str = "https://github.com/hans00/nibwarden";
const MAINTAINER: &str = "Hans <me@hans00.me>";
const LICENSE: &str = "GPL-3.0-only";
const SUMMARY: &str = "Bitwarden-compatible password manager for the Linux desktop";
const DESCRIPTION: [&str; 3] = [
    "nibwardend keeps a Bitwarden or Vaultwarden vault unlocked for the user session and \
     serves it to the nibwarden CLI, to nibwarden's windows, to an SSH agent socket and to \
     the official Bitwarden browser extension. Programs are approved in a window before \
     they get secrets.",
    "nibwarden-gui shows the vault (browse, edit, quick access, Send, import and export) \
     and a system tray icon.",
    "nibwarden-ssh-sign lets git sign commits with SSH keys stored in the vault; run \
     \"nibwarden ssh git-setup\" to configure it.",
];

/// Programs built with the release profile, and the GUI, built with its smaller profile.
const BINS: [&str; 4] = [
    "nibwarden",
    "nibwardend",
    "nibwarden-ssh-sign",
    "nibwarden-browser-host",
];
const GUI_BIN: &str = "nibwarden-gui";

#[derive(Parser)]
#[command(about = "nibwarden's project tasks")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Formatting, clippy (warnings are errors) and the unit tests.
    Check {
        /// Also check that the workspace compiles for this target, e.g.
        /// x86_64-pc-windows-gnu or aarch64-apple-darwin (added with rustup if missing).
        /// Crates with C code need that target's C toolchain and headers.
        #[arg(long)]
        target: Vec<String>,
    },
    /// Build the programs for release.
    Build,
    /// Install the programs, the systemd user unit, desktop entry, icons and man pages
    /// under DESTDIR/PREFIX.
    Install {
        /// A staging root to install into (e.g. a package's), instead of /.
        #[arg(long)]
        destdir: Option<PathBuf>,
        #[arg(long, default_value = "/usr/local")]
        prefix: PathBuf,
        /// Use the programs already built (`cargo xtask build`).
        #[arg(long)]
        no_build: bool,
    },
    /// Build the release artifacts into target/dist.
    Dist {
        /// Use the programs already built (`cargo xtask build`).
        #[arg(long)]
        no_build: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::Check { target } => check(&target),
        Task::Build => build(),
        Task::Install {
            destdir,
            prefix,
            no_build,
        } => {
            if !no_build {
                build()?;
            }
            install(destdir.as_deref(), &prefix)
        }
        Task::Dist { no_build } => {
            if !no_build {
                build()?;
            }
            dist()
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .to_owned()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target"))
}

fn cargo() -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root());
    command
}

fn run(command: &mut Command) -> Result<()> {
    eprintln!("$ {}", describe(command));
    let status = command
        .status()
        .with_context(|| format!("cannot run {}", describe(command)))?;
    ensure!(status.success(), "{} failed ({status})", describe(command));
    Ok(())
}

fn output(command: &mut Command) -> Result<String> {
    let out = command
        .output()
        .with_context(|| format!("cannot run {}", describe(command)))?;
    ensure!(
        out.status.success(),
        "{} failed: {}",
        describe(command),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

// --- check ---

fn check(targets: &[String]) -> Result<()> {
    run(cargo().args(["fmt", "--all", "--check"]))?;
    run(cargo().args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    run(cargo().args(["test", "--workspace"]))?;
    if !targets.is_empty() {
        let installed = output(Command::new("rustup").args(["target", "list", "--installed"]))
            .context("checking other targets needs rustup")?;
        for target in targets {
            if !installed.lines().any(|t| t == target) {
                run(Command::new("rustup").args(["target", "add", target]))?;
            }
            run(cargo()
                .args(["clippy", "--workspace", "--all-targets", "--target", target])
                .args(["--", "-D", "warnings"]))?;
        }
    }
    eprintln!("all checks passed");
    Ok(())
}

// --- build and install ---

fn build() -> Result<()> {
    let release = || {
        let mut command = cargo();
        command.env("RUSTFLAGS", release_rustflags());
        command
    };
    run(release().args([
        "build",
        "--release",
        "--locked",
        "-p",
        "nibwarden-cli",
        "-p",
        "nibwarden-daemon",
    ]))?;
    run(release().args([
        "build",
        "--profile",
        "release-gui",
        "--locked",
        "-p",
        "nibwarden-gui",
    ]))
}

/// Release builds name sources relative to the workspace and the crate registry, not by
/// where they were on the build machine (panic messages carry those paths).
fn release_rustflags() -> String {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    let mut flags = std::env::var("RUSTFLAGS").unwrap_or_default();
    if let Some(cargo_home) = cargo_home {
        flags.push_str(&format!(
            " --remap-path-prefix={}=cargo",
            cargo_home.display()
        ));
    }
    flags.push_str(&format!(
        " --remap-path-prefix={}=nibwarden",
        root().display()
    ));
    flags.trim().to_owned()
}

/// One installed file: where it goes under the prefix, its content, and its mode.
struct Entry {
    path: String,
    content: Vec<u8>,
    mode: u32,
}

/// Every file nibwarden installs, for a given prefix (the systemd unit names the daemon
/// by its installed path). The same list goes into every package.
fn layout(prefix: &Path) -> Result<Vec<Entry>> {
    let root = root();
    let read = |path: &str| fs::read(root.join(path)).with_context(|| format!("reading {path}"));
    let target = target_dir();
    let mut files = Vec::new();
    for bin in BINS {
        let path = target.join("release").join(bin);
        files.push(Entry {
            path: format!("bin/{bin}"),
            content: fs::read(&path)
                .with_context(|| format!("reading {}; build first", path.display()))?,
            mode: 0o755,
        });
    }
    let gui = target.join("release-gui").join(GUI_BIN);
    files.push(Entry {
        path: format!("bin/{GUI_BIN}"),
        content: fs::read(&gui)
            .with_context(|| format!("reading {}; build first", gui.display()))?,
        mode: 0o755,
    });

    let unit = String::from_utf8(read("packaging/systemd/nibwardend.service")?)?.replace(
        "/usr/bin/",
        &format!(
            "{}/bin/",
            prefix.display().to_string().trim_end_matches('/')
        ),
    );
    let mut add = |path: &str, content: Vec<u8>| {
        files.push(Entry {
            path: path.to_owned(),
            content,
            mode: 0o644,
        })
    };
    add("lib/systemd/user/nibwardend.service", unit.into_bytes());
    add(
        "share/applications/nibwarden.desktop",
        read("packaging/desktop/nibwarden.desktop")?,
    );
    add(
        "share/metainfo/nibwarden.metainfo.xml",
        read("packaging/desktop/nibwarden.metainfo.xml")?,
    );
    add(
        "share/icons/hicolor/scalable/apps/nibwarden.svg",
        read("packaging/desktop/icons/nibwarden.svg")?,
    );
    for icon in [
        "nibwarden-locked-symbolic.svg",
        "nibwarden-unlocked-symbolic.svg",
    ] {
        add(
            &format!("share/icons/hicolor/symbolic/apps/{icon}"),
            read(&format!("packaging/desktop/icons/{icon}"))?,
        );
    }
    let mut pages: Vec<_> = fs::read_dir(root.join("packaging/man"))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.extension() == Some(OsStr::new("1")))
        .collect();
    pages.sort();
    for page in pages {
        let name = page.file_name().unwrap().to_string_lossy().into_owned();
        add(
            &format!("share/man/man1/{name}.gz"),
            gzip(&fs::read(&page)?)?,
        );
    }
    add(
        "share/doc/nibwarden/examples/60-nibwarden-ssh-agent.conf",
        read("packaging/environment.d/60-nibwarden-ssh-agent.conf")?,
    );
    Ok(files)
}

fn install(destdir: Option<&Path>, prefix: &Path) -> Result<()> {
    ensure!(prefix.is_absolute(), "--prefix must be an absolute path");
    let base = match destdir {
        Some(destdir) => destdir.join(prefix.strip_prefix("/")?),
        None => prefix.to_owned(),
    };
    for entry in layout(prefix)? {
        let path = base.join(&entry.path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, &entry.content).with_context(|| format!("writing {}", path.display()))?;
        set_mode(&path, entry.mode)?;
    }
    eprintln!("installed into {}", base.display());
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::set_permissions(path, fs::Permissions::from_mode(mode))?)
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}

/// gzip without a file name or time, so the same input gives the same bytes.
fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzBuilder::new().write(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

// --- dist ---

/// File times in the packages: SOURCE_DATE_EPOCH, else the last commit's, so rebuilding
/// the same commit gives the same packages.
fn source_date() -> Result<u64> {
    if let Some(epoch) = std::env::var_os("SOURCE_DATE_EPOCH") {
        return Ok(epoch.to_string_lossy().parse()?);
    }
    let time = output(
        Command::new("git")
            .current_dir(root())
            .args(["log", "-1", "--format=%ct"]),
    )?;
    Ok(time.trim().parse()?)
}

/// The newest glibc symbol version the programs use: the oldest glibc they run on.
fn min_glibc(files: &[Entry]) -> Option<String> {
    const MARK: &[u8] = b"GLIBC_2.";
    let mut newest: Option<Vec<u32>> = None;
    for entry in files.iter().filter(|e| e.mode == 0o755) {
        let data = &entry.content;
        let mut at = 0;
        while let Some(i) = find(&data[at..], MARK) {
            let start = at + i + "GLIBC_".len();
            let end = data[start..]
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b'.'))
                .map_or(data.len(), |n| start + n);
            let version: Vec<u32> = std::str::from_utf8(&data[start..end])
                .unwrap_or_default()
                .split('.')
                .filter_map(|n| n.parse().ok())
                .collect();
            if newest.as_ref().is_none_or(|n| version > *n) {
                newest = Some(version);
            }
            at = end;
        }
    }
    newest.map(|v| v.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn dist() -> Result<()> {
    let out = target_dir().join("dist");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let mtime = source_date()?;
    let arch = std::env::consts::ARCH;

    let files = layout(Path::new("/usr"))?;
    let glibc = min_glibc(&files).context("the programs reference no glibc version")?;
    let mut written = vec![
        deb(&out, &files, arch, &glibc, mtime)?,
        rpm(&out, &files, arch, &glibc, mtime)?,
        binary_tarball(&out, arch, mtime)?,
    ];
    let source = source_tarball(&out)?;
    written.push(pkgbuild(&out, &source)?);
    written.push(source);

    let mut sums = String::new();
    for path in &written {
        let name = path.file_name().unwrap().to_string_lossy();
        writeln!(sums, "{}  {name}", sha256(&fs::read(path)?))?;
    }
    fs::write(out.join("SHA256SUMS"), sums)?;
    for path in &written {
        println!("{}", path.display());
    }
    Ok(())
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A tar archive of `files` under `prefix` (e.g. `./usr`), with the directories leading
/// to them, owned by root.
fn tar_of(files: &[Entry], prefix: &str, mtime: u64) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut dirs: Vec<String> = Vec::new();
    for entry in files {
        let mut dir = String::new();
        for part in Path::new(&entry.path).parent().unwrap().components() {
            dir = format!("{dir}{}/", part.as_os_str().to_string_lossy());
            if !dirs.contains(&dir) {
                dirs.push(dir.clone());
            }
        }
    }
    let header = |path: &str, size: u64, mode: u32, kind: tar::EntryType| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(size);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(mtime);
        let _ = header.set_username("root");
        let _ = header.set_groupname("root");
        (header, path.to_owned())
    };
    for dir in std::iter::once(String::new()).chain(dirs) {
        let (mut h, path) = header(
            &format!("{prefix}/{dir}"),
            0,
            0o755,
            tar::EntryType::Directory,
        );
        builder.append_data(&mut h, &path, std::io::empty())?;
    }
    for entry in files {
        let (mut h, path) = header(
            &format!("{prefix}/{}", entry.path),
            entry.content.len() as u64,
            entry.mode,
            tar::EntryType::Regular,
        );
        builder.append_data(&mut h, &path, entry.content.as_slice())?;
    }
    Ok(builder.into_inner()?)
}

fn deb(out: &Path, files: &[Entry], arch: &str, glibc: &str, mtime: u64) -> Result<PathBuf> {
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("no Debian architecture for {other}"),
    };
    let mut files: Vec<Entry> = files
        .iter()
        .map(|e| Entry {
            path: e.path.clone(),
            content: e.content.clone(),
            mode: e.mode,
        })
        .collect();
    files.push(Entry {
        path: "share/doc/nibwarden/copyright".into(),
        content: format!(
            "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n\
             Upstream-Name: nibwarden\nSource: {HOMEPAGE}\n\nFiles: *\n\
             Copyright: nibwarden contributors\nLicense: {LICENSE}\n \
             On Debian systems, the full text of the GNU General Public License version 3\n \
             can be found in /usr/share/common-licenses/GPL-3.\n"
        )
        .into_bytes(),
        mode: 0o644,
    });
    let size_kib: usize = files.iter().map(|e| e.content.len().div_ceil(1024)).sum();
    let description = DESCRIPTION
        .iter()
        .map(|p| wrap(p, 76).replace('\n', "\n "))
        .collect::<Vec<_>>()
        .join("\n .\n ");
    let control = format!(
        "Package: {NAME}\nVersion: {VERSION}\nArchitecture: {arch}\nMaintainer: {MAINTAINER}\n\
         Installed-Size: {size_kib}\n\
         Depends: libc6 (>= {glibc}), libgcc-s1, libfontconfig1, libfreetype6, libxkbcommon0, \
         ca-certificates\n\
         Recommends: openssh-client, git, libwayland-client0, libegl1, libxkbcommon-x11-0, \
         libx11-xcb1, libxcursor1, libxrandr2, libxi6\n\
         Suggests: libnotify-bin\nSection: utils\nPriority: optional\nHomepage: {HOMEPAGE}\n\
         Description: {SUMMARY}\n {description}\n"
    );
    let control_tar = tar_of(
        &[Entry {
            path: "control".into(),
            content: control.into_bytes(),
            mode: 0o644,
        }],
        ".",
        mtime,
    )?;
    let data_tar = tar_of(&files, "./usr", mtime)?;

    let path = out.join(format!("{NAME}_{VERSION}_{arch}.deb"));
    let mut archive = ar::Builder::new(fs::File::create(&path)?);
    for (name, content) in [
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", gzip(&control_tar)?),
        ("data.tar.gz", gzip(&data_tar)?),
    ] {
        let mut header = ar::Header::new(name.as_bytes().to_vec(), content.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(mtime);
        archive.append(&header, content.as_slice())?;
    }
    Ok(path)
}

fn rpm(out: &Path, files: &[Entry], arch: &str, glibc: &str, mtime: u64) -> Result<PathBuf> {
    use rpm::{BuildConfig, CompressionType, Dependency, FileOptions, PackageBuilder};

    let mut package = PackageBuilder::new(NAME, VERSION, LICENSE, arch, SUMMARY);
    package
        .using_config(
            BuildConfig::v4()
                .compression(CompressionType::Gzip)
                .source_date(mtime as u32),
        )
        .release("1")
        .url(HOMEPAGE)
        .packager(MAINTAINER)
        .description(DESCRIPTION.map(|p| wrap(p, 76)).join("\n\n"));
    for entry in files {
        package.with_file_contents(
            entry.content.clone(),
            FileOptions::new(format!("/usr/{}", entry.path)).permissions(entry.mode as u16),
        )?;
    }
    package.with_file_contents(
        fs::read(root().join("LICENSE"))?,
        FileOptions::new("/usr/share/licenses/nibwarden/LICENSE")
            .permissions(0o644)
            .license(),
    )?;
    // The libraries rpmbuild would find on its own, and the files the programs need.
    package.requires(Dependency::any(format!("libc.so.6(GLIBC_{glibc})(64bit)")));
    for dep in [
        "fontconfig",
        "freetype",
        "libxkbcommon",
        "ca-certificates",
        "hicolor-icon-theme",
    ] {
        package.requires(Dependency::any(dep));
    }
    for dep in [
        "libwayland-client",
        "libxkbcommon-x11",
        "libX11-xcb",
        "libXcursor",
        "libXrandr",
        "libXi",
        "mesa-libEGL",
        "openssh-clients",
        "git",
    ] {
        package.recommends(Dependency::any(dep));
    }
    let path = out.join(format!("{NAME}-{VERSION}-1.{arch}.rpm"));
    package.build()?.write_file(&path)?;
    Ok(path)
}

/// The programs and their files laid out as under a prefix, with the README and license:
/// `sudo tar -C /usr/local --strip-components=1 -xzf …` installs it.
fn binary_tarball(out: &Path, arch: &str, mtime: u64) -> Result<PathBuf> {
    let mut files = layout(Path::new("/usr/local"))?;
    for doc in ["README.md", "LICENSE"] {
        files.push(Entry {
            path: format!("share/doc/nibwarden/{doc}"),
            content: fs::read(root().join(doc))?,
            mode: 0o644,
        });
    }
    let name = format!("{NAME}-{VERSION}-{arch}-linux");
    let path = out.join(format!("{name}.tar.gz"));
    fs::write(&path, gzip(&tar_of(&files, &name, mtime)?)?)?;
    Ok(path)
}

/// The committed source as `nibwarden-<version>/`, what the PKGBUILD and source builds
/// start from.
fn source_tarball(out: &Path) -> Result<PathBuf> {
    let dirty = output(
        Command::new("git")
            .current_dir(root())
            .args(["status", "--porcelain"]),
    )?;
    if !dirty.trim().is_empty() {
        eprintln!("warning: uncommitted changes are not in the source tarball");
    }
    let path = out.join(format!("{NAME}-{VERSION}.tar.gz"));
    run(Command::new("git").current_dir(root()).args([
        "archive",
        "--format=tar.gz",
        &format!("--prefix={NAME}-{VERSION}/"),
        "-o",
        &path.display().to_string(),
        "HEAD",
    ]))?;
    Ok(path)
}

/// packaging/arch/PKGBUILD with this version and the source tarball's checksum, ready for
/// the AUR once the tarball is attached to the release.
fn pkgbuild(out: &Path, source: &Path) -> Result<PathBuf> {
    let template = fs::read_to_string(root().join("packaging/arch/PKGBUILD"))?;
    let sum = sha256(&fs::read(source)?);
    let mut text = String::new();
    for line in template.lines() {
        if line.starts_with("pkgver=") {
            text.push_str(&format!("pkgver={VERSION}\n"));
        } else if line.starts_with("sha256sums=") {
            text.push_str(&format!("sha256sums=('{sum}')\n"));
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    let path = out.join("PKGBUILD");
    fs::write(&path, text)?;
    Ok(path)
}

/// Wraps a paragraph at `width` columns.
fn wrap(text: &str, width: usize) -> String {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().unwrap();
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(word.to_owned());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_newest_glibc() {
        let bin = |content: &[u8]| Entry {
            path: "bin/x".into(),
            content: content.to_vec(),
            mode: 0o755,
        };
        let files = [
            bin(b"\0GLIBC_2.17\0GLIBC_2.34\0"),
            bin(b"GLIBC_2.9\0GLIBC_2.28"),
        ];
        assert_eq!(min_glibc(&files).as_deref(), Some("2.34"));
        assert_eq!(min_glibc(&[bin(b"none")]), None);
    }

    #[test]
    fn wraps() {
        assert_eq!(wrap("aa bb cc", 5), "aa bb\ncc");
    }
}
