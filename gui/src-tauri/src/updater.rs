//! Aktualizacje z GitHub Releases.
//!
//! 1. Pobiera informację o najnowszym wydaniu z api.github.com (HTTPS).
//! 2. Pobiera nowy AppImage i plik z sumą SHA-256, sprawdza zgodność.
//! 3. Podmienia plik AppImage, z którego uruchomiono aplikację ($APPIMAGE),
//!    i uruchamia nową wersję.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tauri::Emitter;

const REPO: &str = "mdziura123-cmd/epson-druk";
const ASSET: &str = "Epson-Druk-x86_64.AppImage";

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub notes: String,
    pub page: String,
    /// `false` gdy aplikacja nie działa jako AppImage (wtedy tylko link do strony).
    pub can_install: bool,
}

struct Release {
    tag: String,
    notes: String,
    page: String,
    appimage_url: Option<String>,
    sha_url: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .user_agent("epson-druk-updater")
        .build()
}

fn fetch_latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = match agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None), // brak wydań
        Err(e) => return Err(format!("nie mogę sprawdzić aktualizacji: {e}")),
    };
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    let asset_url = |name: &str| -> Option<String> {
        v["assets"].as_array()?.iter().find_map(|a| {
            if a["name"].as_str()? == name {
                a["browser_download_url"].as_str().map(String::from)
            } else {
                None
            }
        })
    };
    Ok(Some(Release {
        tag: v["tag_name"].as_str().unwrap_or("").to_string(),
        notes: v["body"].as_str().unwrap_or("").to_string(),
        page: v["html_url"].as_str().unwrap_or("").to_string(),
        appimage_url: asset_url(ASSET),
        sha_url: asset_url(&format!("{ASSET}.sha256")),
    }))
}

fn parse_ver(s: &str) -> [u64; 3] {
    let mut out = [0u64; 3];
    for (i, p) in s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).take(3).enumerate() {
        out[i] = p.parse().unwrap_or(0);
    }
    out
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    parse_ver(latest) > parse_ver(current)
}

fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
}

pub fn check(current: &str) -> Result<UpdateInfo, String> {
    let rel = fetch_latest()?;
    let Some(rel) = rel else {
        return Ok(UpdateInfo {
            current: current.into(),
            latest: current.into(),
            available: false,
            notes: String::new(),
            page: format!("https://github.com/{REPO}/releases"),
            can_install: false,
        });
    };
    let available = is_newer(&rel.tag, current) && rel.appimage_url.is_some();
    Ok(UpdateInfo {
        current: current.into(),
        latest: rel.tag.trim_start_matches('v').to_string(),
        available,
        notes: rel.notes,
        page: rel.page,
        can_install: appimage_path().is_some() && rel.sha_url.is_some(),
    })
}

pub fn install(app: &tauri::AppHandle, current: &str) -> Result<String, String> {
    let target = appimage_path()
        .ok_or("aktualizacja w aplikacji działa tylko w wersji AppImage – pobierz nową wersję ze strony")?;
    let rel = fetch_latest()?.ok_or("brak wydań na GitHubie")?;
    if !is_newer(&rel.tag, current) {
        return Err("masz już najnowszą wersję".into());
    }
    let url = rel.appimage_url.ok_or("w wydaniu brakuje pliku AppImage")?;
    let sha_url = rel.sha_url.ok_or("w wydaniu brakuje sumy kontrolnej – aktualizacja przerwana")?;

    // Suma kontrolna
    let sha_text = agent()
        .get(&sha_url)
        .call()
        .map_err(|e| format!("nie mogę pobrać sumy kontrolnej: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let expected = sha_text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if expected.len() != 64 {
        return Err("niepoprawna suma kontrolna w wydaniu".into());
    }

    // Pobieranie do pliku tymczasowego obok obecnego AppImage
    let dir = target.parent().ok_or("zła ścieżka AppImage")?;
    let tmp = dir.join(".Epson-Druk.aktualizacja.tmp");
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("nie mogę pobrać aktualizacji: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let result = (|| -> Result<(), String> {
        let mut file = fs::File::create(&tmp).map_err(|e| {
            format!("nie mogę zapisać w {} ({e}). Przenieś AppImage do katalogu domowego.", dir.display())
        })?;
        let mut reader = resp.into_reader();
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done: u64 = 0;
        let mut last_pct = u64::MAX;
        let mut head = [0u8; 4];
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("przerwane pobieranie: {e}"))?;
            if n == 0 {
                break;
            }
            if done < 4 {
                for (i, b) in buf[..n].iter().enumerate().take((4 - done) as usize) {
                    head[done as usize + i] = *b;
                }
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            if total > 0 {
                let pct = done * 100 / total;
                if pct != last_pct {
                    last_pct = pct;
                    let _ = app.emit("update-progress", pct);
                }
            }
        }
        file.sync_all().ok();
        drop(file);

        if &head != b"\x7fELF" {
            return Err("pobrany plik nie jest programem AppImage".into());
        }
        let got = format!("{:x}", hasher.finalize());
        if got != expected {
            return Err("suma kontrolna się nie zgadza – plik mógł zostać uszkodzony, aktualizacja przerwana".into());
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        // Podmiana pliku (działający program dalej korzysta ze starej kopii do restartu)
        fs::rename(&tmp, &target).map_err(|e| format!("nie mogę podmienić pliku: {e}"))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(rel.tag.trim_start_matches('v').to_string())
}

/// Uruchamia nową wersję (podmieniony plik AppImage) i zamyka bieżącą.
pub fn restart(app: &tauri::AppHandle) -> Result<(), String> {
    let target = appimage_path().ok_or("brak ścieżki AppImage")?;
    let mut cmd = crate::host_command(target.to_str().ok_or("zła ścieżka")?);
    for v in ["APPIMAGE", "APPDIR", "ARGV0", "OWD"] {
        cmd.env_remove(v);
    }
    cmd.spawn().map_err(|e| format!("nie mogę uruchomić nowej wersji: {e}"))?;
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------
// Sterownik Epson ESC/P-R
// ---------------------------------------------------------------------

pub const ESCPR_PAGE: &str =
    "https://support.epson.net/linux/Printer/LSB_less_distribution_pages/en/escpr.php";
const ESCPR_DOWNLOAD_INFO: &str = "https://download.ebz.epson.net/dsc/du/02/DriverDownloadInfo.do?LG2=JA&CN2=US&CTI=176&PRN=Linux%20rpm%2064bit%20package&OSC=LX";

/// Wyszukuje w tekście numery wersji po „escpr-”, np. „escpr-1.8.6-1” → „1.8.6”.
pub fn find_escpr_version(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for (i, _) in text.match_indices("escpr-") {
        let rest = &text[i + 6..];
        let ver: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let ver = ver.trim_end_matches('.').to_string();
        if ver.split('.').count() >= 2 && ver.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            if best.as_deref().map(|b| is_newer(&ver, b)).unwrap_or(true) {
                best = Some(ver);
            }
        }
    }
    best
}

/// Najnowsza wersja sterownika ze strony Epsona (`None` gdy nie da się ustalić).
pub fn latest_escpr() -> Option<String> {
    let text = agent().get(ESCPR_DOWNLOAD_INFO).call().ok()?.into_string().ok()?;
    find_escpr_version(&text)
}

/// Zainstalowana wersja sterownika (rpm), np. „1.8.6”.
pub fn installed_escpr() -> Option<String> {
    let out = crate::host_command("rpm")
        .args(["-q", "--qf", "%{VERSION}", "epson-inkjet-printer-escpr"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty() && v.chars().next()?.is_ascii_digit()).then_some(v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn wersja_sterownika() {
        let t = "<a href=\"x/epson-inkjet-printer-escpr-1.8.6-1.x86_64.rpm\">escpr-1.8.6-1</a> old escpr-1.7.21-1";
        assert_eq!(super::find_escpr_version(t).as_deref(), Some("1.8.6"));
    }

    use super::*;
    #[test]
    fn wersje() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
    }
}
