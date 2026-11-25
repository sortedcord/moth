//! The programs moth runs to talk to YouTube: [yt-dlp](https://github.com/yt-dlp/yt-dlp) and
//! [Deno](https://deno.com) (which yt-dlp needs to solve YouTube's JavaScript challenges).
//!
//! YouTube changes often enough to break yt-dlp every few weeks, and distributions update it
//! slowly, so moth keeps its own copies of the official releases in `$XDG_DATA_HOME/moth/bin`
//! and prefers them to the ones on the system, which stay the fallback. The copies are
//! installed and updated in the background ([`maintain`]), at most once a day, if the user
//! allows it (the "Keep yt-dlp up to date" setting).
//!
//! Downloads are checked against the SHA-256 sums the releases publish before they're used.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use sha2::{Digest as _, Sha256};

/// How often to look for new versions.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A file whose modification time is when the last check finished.
const STAMP: &str = "last-check";

const YTDLP_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const DENO_RELEASE: &str = "https://github.com/denoland/deno/releases/latest/download";

fn dir() -> PathBuf {
    crate::paths::data_dir().join("bin")
}

fn managed(name: &str) -> Option<PathBuf> {
    Some(dir().join(name)).filter(|path| path.is_file())
}

/// What to run for yt-dlp: moth's own copy if it has one, else the system's.
pub fn ytdlp() -> PathBuf {
    managed("yt-dlp").unwrap_or_else(|| "yt-dlp".into())
}

/// Tells yt-dlp to use moth's Deno, if it has one (otherwise yt-dlp finds the system's).
pub fn js_runtime_args() -> Vec<OsString> {
    match managed("deno") {
        Some(deno) => vec!["--js-runtimes".into(), format!("deno:{}", deno.display()).into()],
        None => Vec::new(),
    }
}

/// The names of the release files for this machine, if there are any.
struct Assets {
    ytdlp: &'static str,
    deno: &'static str,
}

fn assets() -> Option<Assets> {
    match std::env::consts::ARCH {
        "x86_64" => Some(Assets { ytdlp: "yt-dlp_linux", deno: "deno-x86_64-unknown-linux-gnu.zip" }),
        "aarch64" => Some(Assets { ytdlp: "yt-dlp_linux_aarch64", deno: "deno-aarch64-unknown-linux-gnu.zip" }),
        _ => None,
    }
}

/// Install yt-dlp and Deno if moth doesn't have them yet, and update them if they weren't
/// checked in the last day. Failing only means keeping what's there (the problem is logged).
pub async fn maintain(http: reqwest::Client) {
    let Some(assets) = assets() else {
        eprintln!("moth: no yt-dlp release for {}; using the system's", std::env::consts::ARCH);
        return;
    };
    let stamp = dir().join(STAMP);
    let installed = managed("yt-dlp").is_some() && managed("deno").is_some();
    if installed && !is_stale(&stamp) {
        return;
    }
    if let Err(err) = std::fs::create_dir_all(dir()) {
        eprintln!("moth: can't create {}: {err}", dir().display());
        return;
    }
    let mut failed = false;
    if managed("yt-dlp").is_none() {
        failed |= report("install yt-dlp", install_ytdlp(&http, assets.ytdlp).await);
    } else {
        failed |= report("update yt-dlp", update("yt-dlp", &["-U"]).await);
    }
    if managed("deno").is_none() {
        failed |= report("install Deno", install_deno(&http, assets.deno).await);
    } else {
        failed |= report("update Deno", update("deno", &["upgrade", "--quiet"]).await);
    }
    if !failed {
        std::fs::write(&stamp, b"").ok();
    }
}

/// Log a failure; whether there was one.
fn report(what: &str, result: Result<()>) -> bool {
    match result {
        Ok(()) => false,
        Err(err) => {
            eprintln!("moth: couldn't {what}: {err:#}");
            true
        }
    }
}

fn is_stale(stamp: &Path) -> bool {
    let checked = std::fs::metadata(stamp).and_then(|meta| meta.modified());
    match checked {
        Ok(time) => SystemTime::now().duration_since(time).is_ok_and(|age| age >= CHECK_EVERY),
        Err(_) => true,
    }
}

async fn install_ytdlp(http: &reqwest::Client, asset: &str) -> Result<()> {
    let bytes = fetch_verified(http, YTDLP_RELEASE, asset, "SHA2-256SUMS").await?;
    place(&dir().join("yt-dlp"), &bytes)?;
    eprintln!("moth: installed yt-dlp {}", version("yt-dlp").await.unwrap_or_default());
    Ok(())
}

async fn install_deno(http: &reqwest::Client, asset: &str) -> Result<()> {
    let archive = fetch_verified(http, DENO_RELEASE, asset, &format!("{asset}.sha256sum")).await?;
    let deno = unzip_file(&archive, "deno")?;
    place(&dir().join("deno"), &deno)?;
    eprintln!("moth: installed Deno {}", version("deno").await.unwrap_or_default());
    Ok(())
}

/// Have an installed tool update itself (`yt-dlp -U`, `deno upgrade`).
async fn update(name: &'static str, args: &'static [&'static str]) -> Result<()> {
    let before = version(name).await;
    let output = tokio::process::Command::new(dir().join(name))
        .args(args)
        .output()
        .await
        .with_context(|| format!("couldn't run {name}"))?;
    if !output.status.success() {
        bail!("{name} {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    let after = version(name).await;
    if before != after {
        eprintln!("moth: updated {name} to {}", after.unwrap_or_default());
    }
    Ok(())
}

async fn version(name: &str) -> Option<String> {
    let output = tokio::process::Command::new(dir().join(name)).arg("--version").output().await.ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // `deno --version` prints several lines; the first has the version.
    let first = text.lines().next()?;
    Some(first.trim().to_string())
}

/// Download `file` of a release and check it against the release's sums file.
async fn fetch_verified(http: &reqwest::Client, release: &str, file: &str, sums: &str) -> Result<Vec<u8>> {
    let get = async |name: &str| -> Result<Vec<u8>> {
        let url = format!("{release}/{name}");
        let response = http.get(&url).send().await.and_then(|response| response.error_for_status());
        let response = response.with_context(|| format!("couldn't fetch {url}"))?;
        Ok(response.bytes().await.with_context(|| format!("couldn't download {url}"))?.to_vec())
    };
    let sums = String::from_utf8(get(sums).await?).context("the checksums aren't text")?;
    let expected = expected_sum(&sums, file).ok_or_else(|| anyhow!("no checksum for {file}"))?;
    let bytes = get(file).await?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&expected) {
        bail!("{file} doesn't match its checksum");
    }
    Ok(bytes)
}

/// The SHA-256 of `file` in a `sha256sum`-style list (`<hash>  <name>` per line; a list for
/// a single file may omit or alter the name).
fn expected_sum(sums: &str, file: &str) -> Option<String> {
    let mut lines = sums.lines().filter_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        Some((hash.trim(), name.trim().trim_start_matches('*')))
    });
    let single = sums.lines().filter(|line| !line.trim().is_empty()).count() == 1;
    lines.find(|(_, name)| name.rsplit('/').next() == Some(file) || single).map(|(hash, _)| hash.to_string())
}

fn unzip_file(archive: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(archive)).context("not a zip archive")?;
    let mut file = archive.by_name(name).with_context(|| format!("no {name} in the archive"))?;
    let mut bytes = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Write an executable to `path`, replacing what's there only once it's complete.
fn place(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let partial = path.with_extension("part");
    let result = (|| -> Result<()> {
        use std::io::Write as _;
        let mut file =
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&partial, path)?;
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&partial).ok();
    }
    result.with_context(|| format!("couldn't write {}", path.display()))
}

/// A command for yt-dlp that runs at low priority (it may run alongside playback), with
/// moth's own Deno if there is one.
pub fn ytdlp_command() -> Command {
    let mut command = Command::new("nice");
    command.args(["-n", "10"]).arg(ytdlp()).args(js_runtime_args());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_files_sum() {
        let sums = "aaa  yt-dlp_linux\nbbb  yt-dlp_linux.zip\nccc  yt-dlp_linux_aarch64\n";
        assert_eq!(expected_sum(sums, "yt-dlp_linux").as_deref(), Some("aaa"));
        assert_eq!(expected_sum(sums, "yt-dlp_linux_aarch64").as_deref(), Some("ccc"));
        assert_eq!(expected_sum(sums, "yt-dlp_macos"), None);
        // A sums file for one file, whatever it calls it.
        assert_eq!(expected_sum("ddd  ./deno.zip\n", "deno-x86_64-unknown-linux-gnu.zip").as_deref(), Some("ddd"));
        assert_eq!(expected_sum("ddd *dist/deno.zip", "deno.zip").as_deref(), Some("ddd"));
    }

    #[test]
    fn a_stamp_goes_stale_after_a_day() {
        let dir = std::env::temp_dir().join(format!("moth-tools-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(STAMP);
        assert!(is_stale(&stamp));
        std::fs::write(&stamp, b"").unwrap();
        assert!(!is_stale(&stamp));
        let old = SystemTime::now() - CHECK_EVERY - Duration::from_secs(60);
        std::fs::File::options().write(true).open(&stamp).unwrap().set_modified(old).unwrap();
        assert!(is_stale(&stamp));
        std::fs::remove_dir_all(&dir).ok();
    }
}
