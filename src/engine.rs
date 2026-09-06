//! The browse tool's engine, shipped with koda.
//!
//! `browse` drives [agent-browser], which is distributed as a self-contained
//! native binary — one per platform — inside an npm tarball. Requiring people to
//! install it separately meant `npm i -g agent-browser` (and therefore Node) as
//! a prerequisite for a tool koda advertises out of the box, and an install that
//! silently left `browse` broken when that step failed.
//!
//! So koda fetches and unpacks it itself: one pinned version, verified against a
//! pinned hash before anything is written, into a directory koda owns. The
//! installer runs `koda browser install`, and a `browse` call with no engine
//! provisions it on the spot — the same code path on macOS, Linux and Windows.
//!
//! [agent-browser]: https://www.npmjs.com/package/agent-browser

use anyhow::{anyhow, bail, Context, Result};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The version koda ships. Pinned rather than "latest": a coding agent that
/// silently pulls a different binary every time it is installed is not something
/// anyone can reproduce or audit.
pub const VERSION: &str = "0.36.0";

/// sha512 of that exact tarball, as published in the npm registry's
/// `dist.integrity`. Checked before a single byte is unpacked, so a compromised
/// mirror or a truncated download fails loudly instead of leaving an executable
/// behind.
const SHA512_B64: &str =
    "Ljjj4nRKUEqtrFF0pgev8lxTfC79tNgPj67sNi6BLnUAWIG8y9Cu2VQIeZ0MY3N2fTQagoBlfQ/pUY/4NWPD3w==";

fn tarball_url() -> String {
    // Overridable for an air-gapped install or a private mirror; the hash check
    // still applies, so a mirror serving something else is still rejected.
    if let Ok(url) = std::env::var("KODA_AGENT_BROWSER_URL") {
        if !url.trim().is_empty() {
            return url;
        }
    }
    format!("https://registry.npmjs.org/agent-browser/-/agent-browser-{VERSION}.tgz")
}

/// The directory koda keeps its own binaries in. Platform-native: the OS's data
/// directory, not a dotfile invented for the purpose.
pub fn bin_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("koda").join("bin"))
}

/// Where koda's own copy of the engine lives, if this platform has a data dir.
pub fn engine_path() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "agent-browser.exe"
    } else {
        "agent-browser"
    };
    Some(bin_dir()?.join(name))
}

/// koda's own copy, if it has been provisioned and is executable.
pub fn installed() -> Option<PathBuf> {
    engine_path().filter(|p| p.is_file())
}

/// Which binary inside the tarball belongs to this machine.
///
/// The tarball carries every platform's build; only one of them is ours, and
/// picking the wrong one produces a file that cannot run rather than an error.
fn asset_name() -> Result<&'static str> {
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "agent-browser-darwin-arm64",
        ("macos", "x86_64") => "agent-browser-darwin-x64",
        ("linux", "aarch64") if is_musl() => "agent-browser-linux-musl-arm64",
        ("linux", "aarch64") => "agent-browser-linux-arm64",
        ("linux", "x86_64") if is_musl() => "agent-browser-linux-musl-x64",
        ("linux", "x86_64") => "agent-browser-linux-x64",
        ("windows", "x86_64") => "agent-browser-win32-x64.exe",
        (os, arch) => bail!(
            "no agent-browser build for {os}/{arch}; install it yourself and set \
             `browser_path` in the config"
        ),
    };
    Ok(name)
}

/// Alpine and friends need the musl build; the glibc one will not start there.
///
/// Detected from the dynamic loader on disk rather than `ldd --version`, which
/// musl does not implement.
fn is_musl() -> bool {
    if Path::new("/etc/alpine-release").exists() {
        return true;
    }
    if has_glibc_loader() {
        return false;
    }
    has_musl_loader()
}

fn has_glibc_loader() -> bool {
    [
        "/lib/x86_64-linux-gnu",
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/ld-linux-aarch64.so.1",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

fn has_musl_loader() -> bool {
    let Ok(entries) = std::fs::read_dir("/lib") else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("ld-musl-"))
}

/// Fetch and unpack the engine into koda's own bin directory.
///
/// Returns the path to the installed binary. Already installed and `force` not
/// set: returns immediately without touching the network.
pub async fn install(force: bool) -> Result<PathBuf> {
    let target = engine_path().ok_or_else(|| anyhow!("no data directory on this system"))?;
    if !force {
        if let Some(existing) = installed() {
            return Ok(existing);
        }
    }
    let asset = asset_name()?;
    let url = tarball_url();
    crate::tel_info!("engine", "fetching agent-browser", "version" => VERSION, "asset" => asset);

    let body = crate::web::fetch_bytes(&url)
        .await
        .with_context(|| format!("downloading {url}"))?;
    verify(&body)?;
    let bytes = extract(&body, asset)?;

    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("engine path has no parent"))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Write beside and rename, the same way file writes work: a half-downloaded
    // executable that is never renamed into place cannot be run by accident.
    let tmp = dir.join(format!("agent-browser.{}.part", std::process::id()));
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", tmp.display()))?;
    }
    std::fs::rename(&tmp, &target).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("installing {}", target.display())
    })?;
    crate::tel_info!(
        "engine", "agent-browser installed",
        "path" => target.display().to_string(), "bytes" => bytes.len(),
    );
    Ok(target)
}

/// Check the download against the pinned hash before anything is unpacked.
fn verify(body: &[u8]) -> Result<()> {
    use sha2::{Digest, Sha512};
    let digest = Sha512::digest(body);
    let expected = b64_decode(SHA512_B64).ok_or_else(|| anyhow!("bad pinned hash"))?;
    if digest.as_slice() != expected.as_slice() {
        bail!(
            "agent-browser {VERSION} failed its integrity check — refusing to install it. \
             The download did not match the hash koda ships; try again, and if it keeps \
             failing, install agent-browser yourself and set `browser_path`."
        );
    }
    Ok(())
}

/// Pull one file out of the npm tarball. Everything in an npm tarball lives
/// under `package/`, and only our platform's binary is worth unpacking.
fn extract(body: &[u8], asset: &str) -> Result<Vec<u8>> {
    let want = format!("package/bin/{asset}");
    let gz = flate2::read::GzDecoder::new(body);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().context("reading the tarball")? {
        let mut entry = entry.context("reading a tarball entry")?;
        let path = entry.path().context("bad path in the tarball")?;
        if path.to_string_lossy() != want {
            continue;
        }
        let mut out = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut out)
            .context("unpacking the engine")?;
        if out.is_empty() {
            bail!("{want} in the tarball is empty");
        }
        return Ok(out);
    }
    bail!("{want} is not in the agent-browser {VERSION} tarball")
}

/// Standard base64 decode. One pinned hash is the only thing koda decodes, so a
/// dependency for it would be a dependency to read 88 characters.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash guard is the only thing standing between a bad mirror and an
    /// executable on disk, so it has to actually reject.
    #[test]
    fn a_tampered_download_is_refused() {
        let err = verify(b"not the tarball").unwrap_err().to_string();
        assert!(err.contains("integrity check"), "{err}");
        // And the base64 the check rests on must decode to a real sha512.
        assert_eq!(b64_decode(SHA512_B64).map(|d| d.len()), Some(64));
    }

    #[test]
    fn base64_decodes_like_the_standard_alphabet() {
        assert_eq!(b64_decode("").unwrap(), Vec::<u8>::new());
        assert_eq!(b64_decode("QQ==").unwrap(), b"A");
        assert_eq!(b64_decode("QUI=").unwrap(), b"AB");
        assert_eq!(b64_decode("QUJD").unwrap(), b"ABC");
        assert_eq!(b64_decode("+/8=").unwrap(), vec![0xfb, 0xff]);
        assert!(b64_decode("not base64!").is_none());
    }

    /// Every platform koda builds for must map to a real asset in the tarball,
    /// or `browse` is broken there in a way no test would otherwise catch.
    #[test]
    fn this_platform_maps_to_an_asset() {
        let name = asset_name().expect("koda builds for this platform");
        assert!(name.starts_with("agent-browser-"), "{name}");
        if cfg!(windows) {
            assert!(name.ends_with(".exe"), "{name}");
        }
    }

    /// The engine lives in a directory koda owns, next to nothing else.
    #[test]
    fn the_engine_path_is_under_kodas_data_dir() {
        let Some(p) = engine_path() else { return };
        assert!(
            p.ends_with("koda/bin/agent-browser") || p.ends_with("koda\\bin\\agent-browser.exe")
        );
    }

    /// Extraction must take our platform's binary and nothing else.
    #[test]
    fn extract_finds_the_platform_binary() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, body) in [
                ("package/bin/agent-browser-linux-x64", &b"LINUX"[..]),
                ("package/bin/agent-browser-darwin-arm64", &b"MAC"[..]),
                ("package/package.json", &b"{}"[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, body).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use std::io::Write as _;
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }
        assert_eq!(extract(&gz, "agent-browser-linux-x64").unwrap(), b"LINUX");
        assert_eq!(extract(&gz, "agent-browser-darwin-arm64").unwrap(), b"MAC");
        let err = extract(&gz, "agent-browser-win32-x64.exe")
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not in the agent-browser"), "{err}");
    }
}
