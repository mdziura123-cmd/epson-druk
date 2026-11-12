//! Drukowanie przez systemowy CUPS i sterownik Epson ESC/P-R
//! (dla drukarek bez IPP, np. Epson L3150 – port 9100).

use crate::host_command;
use std::path::Path;

/// Opcje, których nigdy nie przekazujemy do `lp` (kolizja z opcjami obrazu filtrów CUPS).
const BLOCKED_OPTIONS: &[&str] = &[
    "Brightness", "Contrast", "Saturation", "Hue", "Gamma", "Sharpness",
    "Ink", "ColorModel", "ColorMode", "PageRegion",
];

/// Nazwa kolejki tworzonej przez aplikację.
pub const QUEUE: &str = "Epson_Druk";

/// Znacznik błędu: brak sterownika – okienko pokazuje wtedy instrukcję instalacji.
pub const NO_DRIVER: &str = "BRAK_STEROWNIKA";

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = host_command(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("nie mogę uruchomić {program}: {e} (czy CUPS jest zainstalowany?)"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{program} zakończył się błędem") } else { err })
    }
}

/// Krótki symbol modelu do szukania sterownika, np. "EPSON L3150 Series" → "L3150".
fn model_key(model: &str) -> String {
    model
        .split_whitespace()
        .find(|w| w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_ascii_alphabetic()))
        .unwrap_or("L3150")
        .to_string()
}

/// Sterownik: nazwa modelu z `lpinfo -m` albo bezpośrednia ścieżka do pliku PPD.
pub enum Driver {
    Model(String),
    File(String),
}

/// Szuka pliku PPD na dysku (sterownik Epsona z RPM instaluje się w /opt
/// i nie zawsze jest widoczny w `lpinfo -m`).
fn find_ppd_file(key: &str) -> Option<String> {
    fn walk(dir: &Path, key: &str, depth: u32, out: &mut Vec<String>) {
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, key, depth - 1, out);
            } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                let l = n.to_lowercase();
                if l.contains(key) && (l.ends_with(".ppd") || l.ends_with(".ppd.gz")) {
                    out.push(p.to_string_lossy().to_string());
                }
            }
        }
    }
    let mut found = Vec::new();
    for d in [
        "/opt/epson-inkjet-printer-escpr",
        "/usr/lib/opt/epson-inkjet-printer-escpr",
        "/var/opt/epson-inkjet-printer-escpr",
        "/usr/share/ppd",
        "/usr/share/cups/model",
    ] {
        walk(Path::new(d), key, 5, &mut found);
    }
    found.sort_by_key(|p| (!p.contains("-en."), p.len()));
    found.into_iter().next()
}

/// Szuka sterownika dla modelu. Preferuje Epson ESC/P-R.
pub fn find_driver(model: &str) -> Option<Driver> {
    find_driver_model(model)
        .map(Driver::Model)
        .or_else(|| find_ppd_file(&model_key(model).to_lowercase()).map(Driver::File))
}

fn find_driver_model(model: &str) -> Option<String> {
    let key = model_key(model).to_lowercase();
    let list = run("lpinfo", &["-m"]).ok()?;
    let mut matches: Vec<&str> = list
        .lines()
        .filter(|l| l.to_lowercase().contains(&key))
        .collect();
    matches.sort_by_key(|l| {
        let l = l.to_lowercase();
        (!l.contains("escpr"), !l.contains("-en"), l.len())
    });
    matches
        .first()
        .and_then(|l| l.split_whitespace().next())
        .map(String::from)
}

/// Adres urządzenia ustawiony w kolejce (np. socket://192.168.1.50:9100).
pub fn queue_uri() -> Option<String> {
    let out = run("lpstat", &["-v", QUEUE]).ok()?;
    out.lines()
        .find_map(|l| l.split_once(": ").map(|(_, u)| u.trim().to_string()))
}

pub fn uri_for(ip: &str) -> String {
    format!("socket://{ip}:9100")
}

pub fn queue_ready_for(ip: &str) -> bool {
    queue_uri().as_deref() == Some(uri_for(ip).as_str())
}

/// Tworzy (lub poprawia adres) kolejki CUPS dla drukarki.
pub fn setup(ip: &str, model: &str) -> Result<(), String> {
    let (flag, ppd) = match find_driver(model).ok_or_else(|| NO_DRIVER.to_string())? {
        Driver::Model(m) => ("-m", m),
        Driver::File(f) => ("-P", f),
    };
    let uri = uri_for(ip);
    let args = [
        "-p", QUEUE, "-E", "-v", uri.as_str(), flag, ppd.as_str(),
        "-D", model, "-L", "Wi-Fi (Epson Druk)",
    ];
    match run("lpadmin", &args) {
        Ok(_) => {}
        Err(e) => {
            let low = e.to_lowercase();
            if low.contains("forbidden") || low.contains("unauthorized") || low.contains("password") {
                // Okno systemowe z prośbą o hasło
                let mut pk = vec!["lpadmin"];
                pk.extend_from_slice(&args);
                run("pkexec", &pk).map_err(|e| format!("nie udało się dodać drukarki: {e}"))?;
            } else {
                return Err(format!("nie udało się dodać drukarki: {e}"));
            }
        }
    }
    let _ = run("cupsenable", &[QUEUE]);
    let _ = run("cupsaccept", &[QUEUE]);
    Ok(())
}

/// Stan kolejki: (stan słownie, komunikat).
pub fn state() -> (String, String) {
    let out = run("lpstat", &["-p", QUEUE]).unwrap_or_default();
    let low = out.to_lowercase();
    let state = if low.contains("disabled") {
        "zatrzymana"
    } else if low.contains("printing") {
        "drukuje"
    } else if low.contains("idle") {
        "gotowa"
    } else {
        "nieznany"
    };
    let message = out
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (state.into(), message)
}

/// Opcja sterownika (z `lpoptions -l`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriverOption {
    pub key: String,
    pub label: String,
    pub default: String,
    pub choices: Vec<String>,
}

pub fn parse_options(out: &str) -> Vec<DriverOption> {
    out.lines()
        .filter_map(|line| {
            let (head, vals) = line.split_once(':')?;
            let mut hp = head.splitn(2, '/');
            let key = hp.next()?.trim().to_string();
            let label = hp.next().unwrap_or(&key).trim().to_string();
            let mut default = String::new();
            let choices: Vec<String> = vals
                .split_whitespace()
                .map(|v| {
                    if let Some(d) = v.strip_prefix('*') {
                        default = d.to_string();
                        d.to_string()
                    } else {
                        v.to_string()
                    }
                })
                .collect();
            (!key.is_empty() && choices.len() > 1).then_some(DriverOption { key, label, default, choices })
        })
        .collect()
}

/// Wszystkie opcje sterownika kolejki.
pub fn options() -> Vec<DriverOption> {
    run("lpoptions", &["-p", QUEUE, "-l"])
        .map(|o| parse_options(&o))
        .unwrap_or_default()
}

/// Opcja koloru ze sterownika: (nazwa, wartość kolor, wartość czarno-biała).
fn color_option() -> Option<(String, String, String)> {
    let out = run("lpoptions", &["-p", QUEUE, "-l"]).ok()?;
    for line in out.lines() {
        let (head, vals) = line.split_once(':')?;
        let name = head.split('/').next()?.trim().to_string();
        let values: Vec<String> = vals
            .split_whitespace()
            .map(|v| v.trim_start_matches('*').to_string())
            .collect();
        let mono = values.iter().find(|v| {
            let v = v.to_lowercase();
            v.contains("mono") || v.contains("gray") || v.contains("grey") || v == "black"
        });
        let color = values.iter().find(|v| {
            let v = v.to_lowercase();
            v.contains("color") || v.contains("colour") || v == "rgb" || v == "cmyk"
        });
        let n = name.to_lowercase();
        if let (Some(m), Some(c)) = (mono, color) {
            if n.contains("ink") || n.contains("color") || n.contains("colour") || n.contains("mode") {
                return Some((name, c.clone(), m.clone()));
            }
        }
    }
    None
}

/// Drukuje plik przez CUPS. Zwraca numer zadania.
pub fn print(path: &Path, copies: u32, color: bool, fit: bool) -> Result<i32, String> {
    print_ex(path, None, copies, color, fit, &[])
}

/// Drukuje z dodatkowymi opcjami sterownika (np. MediaType=PMPHOTO).
pub fn print_ex(
    path: &Path,
    title: Option<&str>,
    copies: u32,
    color: bool,
    fit: bool,
    extra: &[(String, String)],
) -> Result<i32, String> {
    let file = path.to_str().ok_or("zła ścieżka pliku")?;
    let title = title
        .map(String::from)
        .unwrap_or_else(|| path.file_name().and_then(|n| n.to_str()).unwrap_or("dokument").to_string());
    let n = copies.clamp(1, 99).to_string();
    let mut args: Vec<String> = vec![
        "-d".into(), QUEUE.into(), "-n".into(), n, "-t".into(), title,
    ];
    if fit {
        args.push("-o".into());
        args.push("fit-to-page".into());
    }
    let known = options();
    for (k, v) in extra {
        // Tych nazw nie wysyłamy nigdy: filtry CUPS traktują je jako własne opcje obrazu
        // w procentach (np. Brightness=0 daje całkowicie czarną stronę).
        if BLOCKED_OPTIONS.iter().any(|b| b.eq_ignore_ascii_case(k)) {
            continue;
        }
        // tylko opcje, które sterownik naprawdę ma, różne od domyślnych, bez znaków specjalnych
        let safe = |x: &str| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        let Some(opt) = known.iter().find(|o| &o.key == k) else { continue };
        if opt.default != *v && opt.choices.iter().any(|c| c == v) && safe(k) && safe(v) {
            args.push("-o".into());
            args.push(format!("{k}={v}"));
        }
    }
    if let Some((name, c, m)) = color_option() {
        args.push("-o".into());
        args.push(format!("{name}={}", if color { c } else { m }));
    } else if !color {
        args.push("-o".into());
        args.push("print-color-mode=monochrome".into());
    }
    args.push("--".into());
    args.push(file.into());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run("lp", &refs).map_err(|e| format!("CUPS odrzucił wydruk: {e}"))?;
    // "request id is Epson_Druk-12 (1 file(s))"
    Ok(out
        .split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{QUEUE}-")).and_then(|n| n.parse().ok()))
        .unwrap_or(0))
}

/// Zadanie z postępem.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobInfo {
    pub id: i32,
    pub name: String,
    /// "w kolejce", "wstrzymane", "drukuje", "gotowe"…
    pub state: String,
    /// 0–100 gdy znany.
    pub progress: Option<u8>,
    /// np. "strona 2 z 4"
    pub detail: String,
    pub can_hold: bool,
    pub can_release: bool,
    pub active: bool,
}

fn state_word(s: i32) -> &'static str {
    match s {
        3 => "w kolejce",
        4 => "wstrzymane",
        5 => "drukuje",
        6 => "zatrzymane",
        7 => "anulowane",
        8 => "przerwane",
        9 => "gotowe",
        _ => "nieznany",
    }
}

/// Zadania z postępem (przez IPP lokalnego CUPS-a); zapas: `lpq`.
pub fn jobs_detailed() -> Vec<JobInfo> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match epson_druk::cups_jobs(QUEUE, "all") {
        Ok(list) => {
            let mut out: Vec<JobInfo> = list
                .into_iter()
                .filter(|j| match j.state {
                    3..=6 => true,
                    // zakończone pokazujemy jeszcze przez 2 minuty
                    9 => j.completed_at.map(|t| now - t < 120).unwrap_or(false),
                    7 | 8 => j.completed_at.map(|t| now - t < 30).unwrap_or(false),
                    _ => false,
                })
                .map(|j| {
                    let progress = match (j.state, j.pages, j.pages_done) {
                        (9, _, _) => Some(100),
                        (5, Some(t), Some(d)) if t > 0 => Some(((d.min(t) * 100) / t) as u8),
                        (5, _, _) => None,
                        _ => Some(0),
                    };
                    let detail = match (j.state, j.pages, j.pages_done) {
                        (5, Some(t), d) => format!("strona {} z {t}", (d.unwrap_or(0) + 1).min(t)),
                        (5, None, _) => {
                            if j.message.is_empty() { "wysyłanie do drukarki…".into() } else { j.message.clone() }
                        }
                        (_, Some(t), _) => format!("{t} str."),
                        _ => String::new(),
                    };
                    JobInfo {
                        id: j.id,
                        name: j.name,
                        state: state_word(j.state).into(),
                        progress,
                        detail,
                        can_hold: j.state == 3,
                        can_release: j.state == 4,
                        active: j.state == 5,
                    }
                })
                .collect();
            out.sort_by_key(|j| (!j.active, j.id));
            out
        }
        Err(_) => jobs()
            .into_iter()
            .map(|(id, name, state)| {
                let active = state == "drukuje";
                JobInfo { id, name, state: state.clone(), progress: None, detail: String::new(), can_hold: !active, can_release: false, active }
            })
            .collect(),
    }
}

pub fn hold(id: i32) -> Result<(), String> {
    run("lp", &["-i", &format!("{QUEUE}-{id}"), "-H", "hold"]).map(|_| ())
}

pub fn release(id: i32) -> Result<(), String> {
    run("lp", &["-i", &format!("{QUEUE}-{id}"), "-H", "resume"]).map(|_| ())
}

/// Lista zadań: (numer, nazwa, stan).
pub fn jobs() -> Vec<(i32, String, String)> {
    let out = run("lpq", &["-P", QUEUE]).unwrap_or_default();
    let mut list = Vec::new();
    let mut in_table = false;
    for line in out.lines() {
        if line.starts_with("Rank") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() < 5 {
            continue;
        }
        let Ok(id) = w[2].parse::<i32>() else { continue };
        // Rank Owner Job File(s)… Size "bytes"
        let name = w[3..w.len().saturating_sub(2)].join(" ");
        let state = if w[0] == "active" { "drukuje" } else { "w kolejce" };
        list.push((id, name, state.to_string()));
    }
    list
}

pub fn cancel(id: i32) -> Result<(), String> {
    run("cancel", &[&format!("{QUEUE}-{id}")]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opcje() {
        let o = parse_options("PageSize/Paper Size: *A4 A5 T4X6\nMediaType/Media Type: *PLAIN PMPHOTO\nInk/Ink: COLOR *MONO\nX/One: *A");
        assert_eq!(o.len(), 3);
        assert_eq!(o[0].key, "PageSize");
        assert_eq!(o[0].label, "Paper Size");
        assert_eq!(o[0].default, "A4");
        assert_eq!(o[2].default, "MONO");
    }

    #[test]
    fn klucz_modelu() {
        assert_eq!(model_key("EPSON L3150 Series"), "L3150");
        assert_eq!(model_key("EPSON"), "L3150");
    }
}
