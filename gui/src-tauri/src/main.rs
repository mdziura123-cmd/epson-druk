//! Epson Druk – aplikacja z okienkiem (Tauri 2).

mod cups;
mod photo;
mod updater;

use epson_druk as ipp;
use serde::Serialize;
use std::fs;
use std::io::Cursor;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;

// ---------------------------------------------------------------------
// Dane przekazywane do okienka
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct FoundDto {
    ip: String,
    model: Option<String>,
    ipp: bool,
    raw: bool,
}

#[derive(Serialize)]
struct InkDto {
    name: String,
    level: Option<u8>,
    color: Option<String>,
}

#[derive(Serialize)]
struct StatusDto {
    ip: String,
    model: String,
    state: String,
    reasons: Vec<String>,
    message: String,
    inks: Vec<InkDto>,
    formats: Vec<String>,
    color_supported: bool,
    /// "ipp" – bezpośrednio, "cups" – przez sterownik (port 9100).
    mode: String,
    /// Tryb cups: kolejka w systemie ustawiona na ten adres.
    queue_ready: bool,
    /// Tryb cups: brak sterownika Epsona w systemie.
    driver_missing: bool,
    /// Wersja oprogramowania drukarki (SNMP), jeśli dostępna.
    firmware: Option<String>,
    /// Czy poziomy tuszu pochodzą z drukarki (SNMP / IPP).
    ink_live: bool,
    /// Drukarka odpowiada, ale nie mierzy tuszu (np. zawsze 100%).
    ink_unmeasured: bool,
}

#[derive(Serialize)]
struct DriverInfoDto {
    installed: Option<String>,
    latest: Option<String>,
    update_available: bool,
    page: String,
}

#[derive(Serialize)]
struct JobDto {
    id: i32,
    name: String,
    state: String,
    progress: Option<u8>,
    detail: String,
    can_hold: bool,
    can_release: bool,
    active: bool,
}

impl JobDto {
    fn simple(id: i32, name: String, state: String) -> Self {
        let active = state == "drukuje";
        JobDto { id, name, state, progress: None, detail: String::new(), can_hold: false, can_release: false, active }
    }
}

#[derive(Serialize)]
struct PrintResultDto {
    jobs: Vec<i32>,
    note: String,
}

// ---------------------------------------------------------------------
// Pomocnicze
// ---------------------------------------------------------------------

fn parse_ip(s: &str) -> Result<IpAddr, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("„{s}” nie jest poprawnym adresem IP"))
}

/// Uruchamia funkcję blokującą (sieć, pliki) poza wątkiem okienka.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("błąd wewnętrzny: {e}"))?
}

/// Polecenie systemowe bez zmiennych środowiska AppImage
/// (inaczej programy z systemu mogłyby załadować złe biblioteki).
fn host_command(program: &str) -> Command {
    let mut c = Command::new(program);
    for v in [
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "GDK_PIXBUF_MODULE_FILE",
        "GDK_PIXBUF_MODULEDIR",
        "GIO_MODULE_DIR",
        "GTK_PATH",
        "GTK_EXE_PREFIX",
        "GTK_DATA_PREFIX",
        "GSETTINGS_SCHEMA_DIR",
        "PYTHONHOME",
        "PYTHONPATH",
    ] {
        c.env_remove(v);
    }
    c
}

fn temp_dir() -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("epson-druk-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("nie mogę utworzyć katalogu tymczasowego: {e}"))?;
    Ok(dir)
}

/// Zamienia PDF na strony JPEG (300 dpi) przy pomocy pdftoppm albo Ghostscripta.
fn pdf_to_jpegs(pdf: &Path) -> Result<(Vec<Vec<u8>>, PathBuf), String> {
    let dir = temp_dir()?;
    let prefix = dir.join("strona");

    let ok_pdftoppm = host_command("pdftoppm")
        .args(["-jpeg", "-jpegopt", "quality=92", "-r", "300"])
        .arg(pdf)
        .arg(&prefix)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ok_pdftoppm {
        let out = dir.join("strona-%03d.jpg");
        let ok_gs = host_command("gs")
            .args(["-q", "-dNOPAUSE", "-dBATCH", "-dSAFER", "-sDEVICE=jpeg", "-r300", "-dJPEGQ=92"])
            .arg(format!("-sOutputFile={}", out.display()))
            .arg(pdf)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok_gs {
            let _ = fs::remove_dir_all(&dir);
            return Err(
                "drukarka nie przyjmuje PDF bezpośrednio, a w systemie brakuje programu do \
                 konwersji (pdftoppm lub gs). Na Bazzite możesz doinstalować: \
                 rpm-ostree install poppler-utils"
                    .into(),
            );
        }
    }

    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "jpg").unwrap_or(false))
        .collect();
    files.sort();
    if files.is_empty() {
        let _ = fs::remove_dir_all(&dir);
        return Err("konwersja PDF nie dała żadnych stron".into());
    }
    let pages = files
        .iter()
        .map(|f| fs::read(f).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((pages, dir))
}

/// Zamienia dowolny obsługiwany obraz (PNG, WEBP, BMP, GIF…) na JPEG.
fn image_to_jpeg(data: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(data).map_err(|e| format!("nie mogę odczytać obrazu: {e}"))?;
    let rgb = img.to_rgb8();
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
        .encode_image(&rgb)
        .map_err(|e| format!("nie mogę zapisać JPEG: {e}"))?;
    Ok(out.into_inner())
}

fn config_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("brak katalogu ustawień: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("drukarka.txt"))
}

// ---------------------------------------------------------------------
// Komendy wywoływane z okienka
// ---------------------------------------------------------------------

#[tauri::command]
fn load_printer(app: tauri::AppHandle) -> Option<String> {
    let f = config_file(&app).ok()?;
    let s = fs::read_to_string(f).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[tauri::command]
fn save_printer(app: tauri::AppHandle, ip: String) -> Result<(), String> {
    parse_ip(&ip)?;
    fs::write(config_file(&app)?, ip.trim()).map_err(|e| format!("nie mogę zapisać ustawień: {e}"))
}

#[tauri::command]
async fn scan(extra: Option<String>) -> Result<Vec<FoundDto>, String> {
    let extra_v4 = extra
        .as_deref()
        .and_then(|s| s.trim().parse::<std::net::Ipv4Addr>().ok());
    let found = blocking(move || ipp::scan_with(extra_v4)).await?;
    Ok(found
        .into_iter()
        .map(|f| FoundDto { ip: f.ip.to_string(), model: f.model, ipp: f.ipp, raw: f.raw })
        .collect())
}

fn port_open(ip: IpAddr, port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::new(ip, port),
        std::time::Duration::from_millis(1500),
    )
    .is_ok()
}

/// Czy drukarka obsługuje IPP (port 631)? Jeśli nie – drukujemy przez CUPS.
fn uses_ipp(ip: IpAddr) -> bool {
    port_open(ip, ipp::IPP_PORT)
}

static FIRMWARE: std::sync::Mutex<Option<(IpAddr, Option<String>)>> = std::sync::Mutex::new(None);

/// Wersja firmware – pytamy drukarkę tylko raz na adres.
fn firmware_cached(ip: IpAddr) -> Option<String> {
    let mut g = FIRMWARE.lock().ok()?;
    if let Some((cip, fw)) = g.as_ref() {
        if *cip == ip {
            return fw.clone();
        }
    }
    let fw = ipp::snmp::firmware_version(ip);
    *g = Some((ip, fw.clone()));
    fw
}

/// Uzupełnia stan o dane z SNMP (tusz, brak papieru, zacięcie…).
fn merge_snmp(ip: IpAddr, dto: &mut StatusDto) {
    let Some(dev) = ipp::snmp::device_state(ip) else { return };
    // EcoTank bez czujników (np. L3150) zgłasza stale 100% – to nie jest prawdziwy pomiar.
    let fake_full = !dev.inks.is_empty() && dev.inks.iter().all(|(_, p, _)| *p == 100);
    if fake_full {
        dto.ink_unmeasured = true;
        dto.inks = dev
            .inks
            .iter()
            .map(|(name, _, color)| InkDto { name: name.clone(), level: None, color: Some(color.clone()) })
            .collect();
    } else if !dev.inks.is_empty() && (dto.inks.is_empty() || dto.inks.iter().all(|i| i.level.is_none())) {
        dto.inks = dev
            .inks
            .into_iter()
            .map(|(name, pct, color)| InkDto { name, level: Some(pct), color: Some(color) })
            .collect();
        dto.ink_live = true;
    }
    for r in dev.reasons {
        if !dto.reasons.contains(&r) {
            dto.reasons.push(r);
        }
    }
    if let Some(st) = dev.state {
        if st == "czyszczenie" || st == "błąd" || dto.state == "nieznany" || dto.state == "wymaga konfiguracji" && dto.queue_ready {
            dto.state = st;
        }
    }
}

fn cups_status(ip: IpAddr, ip_str: String) -> Result<StatusDto, String> {
    if !port_open(ip, ipp::RAW_PORT) {
        return Err(format!(
            "drukarka pod adresem {ip} nie odpowiada. Sprawdź, czy jest włączona i czy router \
             nie blokuje połączenia (izolacja sieci smart home)."
        ));
    }
    let model = ipp::web_model(ip).unwrap_or_else(|| "EPSON L3150 Series".into());
    let ready = cups::queue_ready_for(&ip_str);
    let (state, message, driver_missing) = if ready {
        let (st, msg) = cups::state();
        (st, msg, false)
    } else {
        let missing = cups::find_driver(&model).is_none();
        ("wymaga konfiguracji".to_string(), String::new(), missing)
    };
    Ok(StatusDto {
        ip: ip_str,
        model,
        state,
        reasons: Vec::new(),
        message,
        inks: Vec::new(),
        formats: Vec::new(),
        color_supported: true,
        mode: "cups".into(),
        queue_ready: ready,
        driver_missing,
        firmware: None,
        ink_live: false,
        ink_unmeasured: false,
    })
}

#[tauri::command]
async fn maintenance(ip: String, action: String) -> Result<(), String> {
    let addr = parse_ip(&ip)?;
    let m = match action.as_str() {
        "nozzle" => ipp::Maintenance::NozzleCheck,
        "clean" => ipp::Maintenance::HeadClean,
        _ => return Err("nieznana operacja".into()),
    };
    blocking(move || ipp::maintenance(addr, m)).await
}

#[tauri::command]
async fn setup_printer(ip: String, model: String) -> Result<(), String> {
    parse_ip(&ip)?;
    blocking(move || cups::setup(ip.trim(), &model)).await
}

#[tauri::command]
async fn status(ip: String) -> Result<StatusDto, String> {
    let addr = parse_ip(&ip)?;
    let mut dto = status_inner(ip).await?;
    blocking(move || {
        merge_snmp(addr, &mut dto);
        if dto.mode == "ipp" && dto.inks.iter().any(|i| i.level.is_some()) {
            dto.ink_live = true;
        }
        dto.firmware = firmware_cached(addr);
        Ok(dto)
    })
    .await
}

async fn status_inner(ip: String) -> Result<StatusDto, String> {
    let addr = parse_ip(&ip)?;
    let ip2 = ip.clone();
    let s = match blocking(move || {
        if uses_ipp(addr) {
            ipp::status(addr).map(Ok)
        } else {
            cups_status(addr, ip2).map(Err)
        }
    })
    .await?
    {
        Ok(s) => s,
        Err(cups_dto) => return Ok(cups_dto),
    };
    Ok(StatusDto {
        ip,
        model: s.model,
        state: s.state,
        reasons: s.reasons,
        message: s.message,
        inks: s
            .inks
            .into_iter()
            .map(|i| InkDto { name: i.name, level: i.level, color: i.color })
            .collect(),
        formats: s.formats,
        color_supported: s.color_supported,
        mode: "ipp".into(),
        queue_ready: true,
        driver_missing: false,
        firmware: None,
        ink_live: false,
        ink_unmeasured: false,
    })
}

#[tauri::command]
async fn jobs(ip: String) -> Result<Vec<JobDto>, String> {
    let addr = parse_ip(&ip)?;
    blocking(move || {
        if uses_ipp(addr) {
            Ok(ipp::jobs(addr)?
                .into_iter()
                .map(|j| JobDto::simple(j.id, j.name, j.state))
                .collect())
        } else {
            Ok(cups::jobs_detailed()
                .into_iter()
                .map(|j| JobDto {
                    id: j.id,
                    name: j.name,
                    state: j.state,
                    progress: j.progress,
                    detail: j.detail,
                    can_hold: j.can_hold,
                    can_release: j.can_release,
                    active: j.active,
                })
                .collect())
        }
    })
    .await
}

#[tauri::command]
async fn cancel(ip: String, id: i32) -> Result<(), String> {
    let addr = parse_ip(&ip)?;
    blocking(move || {
        if uses_ipp(addr) {
            ipp::cancel_job(addr, id)
        } else {
            cups::cancel(id)
        }
    })
    .await
}

#[tauri::command]
async fn print_file(
    ip: String,
    path: String,
    copies: u32,
    color: bool,
    fit: bool,
    options: Option<std::collections::HashMap<String, String>>,
    photo: Option<photo::PhotoParams>,
) -> Result<PrintResultDto, String> {
    let addr = parse_ip(&ip)?;
    blocking(move || {
        let src = PathBuf::from(&path);
        let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("dokument").to_string();
        // Zdjęcie z ustawieniami → gotowy JPEG w katalogu tymczasowym
        let mut tmp_dir: Option<PathBuf> = None;
        let mut file = src.clone();
        if let Some(p) = photo.as_ref().filter(|_| photo::is_photo(&src)) {
            let mut p = p.clone();
            p.color = p.color && color;
            let jpg = photo::render_for_print(&src, &p)?;
            let dir = temp_dir()?;
            let out = dir.join("zdjecie.jpg");
            fs::write(&out, jpg).map_err(|e| format!("nie mogę zapisać zdjęcia: {e}"))?;
            file = out;
            tmp_dir = Some(dir);
        }
        let result = if uses_ipp(addr) {
            if tmp_dir.is_some() {
                let data = fs::read(&file).map_err(|e| e.to_string())?;
                let opts = ipp::PrintOptions { copies: copies.max(1), color, fit };
                ipp::print(addr, &data, "image/jpeg", &name, &opts)
                    .map(|id| PrintResultDto { jobs: vec![id], note: String::new() })
            } else {
                do_print(addr, &file, copies, color, fit)
            }
        } else if !cups::queue_ready_for(ip.trim()) {
            Err("najpierw kliknij „Skonfiguruj drukarkę” w karcie Drukarka".into())
        } else {
            let extra: Vec<(String, String)> = options.unwrap_or_default().into_iter().collect();
            cups::print_ex(&file, Some(&name), copies, color, fit, &extra)
                .map(|id| PrintResultDto { jobs: vec![id], note: String::new() })
        };
        if let Some(d) = tmp_dir {
            let _ = fs::remove_dir_all(d);
        }
        result
    })
    .await
}

#[tauri::command]
async fn photo_preview(path: String, photo: photo::PhotoParams) -> Result<String, String> {
    blocking(move || photo::preview(Path::new(&path), &photo)).await
}

#[tauri::command]
async fn printer_options(ip: String) -> Result<Vec<cups::DriverOption>, String> {
    let addr = parse_ip(&ip)?;
    blocking(move || {
        if uses_ipp(addr) || !cups::queue_ready_for(ip.trim()) {
            Ok(Vec::new())
        } else {
            Ok(cups::options())
        }
    })
    .await
}

#[tauri::command]
async fn job_action(ip: String, id: i32, action: String) -> Result<(), String> {
    let addr = parse_ip(&ip)?;
    blocking(move || {
        if uses_ipp(addr) {
            return match action.as_str() {
                "cancel" => ipp::cancel_job(addr, id),
                _ => Err("ta drukarka pozwala tylko anulować zadania".into()),
            };
        }
        match action.as_str() {
            "hold" => cups::hold(id),
            "release" => cups::release(id),
            "cancel" => cups::cancel(id),
            _ => Err("nieznana operacja".into()),
        }
    })
    .await
}

fn setting_file(app: &tauri::AppHandle, key: &str) -> Result<PathBuf, String> {
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("zła nazwa ustawienia".into());
    }
    Ok(config_file(app)?.with_file_name(format!("{key}.json")))
}

#[tauri::command]
fn load_setting(app: tauri::AppHandle, key: String) -> Option<String> {
    fs::read_to_string(setting_file(&app, &key).ok()?).ok()
}

#[tauri::command]
fn save_setting(app: tauri::AppHandle, key: String, value: String) -> Result<(), String> {
    if value.len() > 64 * 1024 {
        return Err("za duże ustawienie".into());
    }
    fs::write(setting_file(&app, &key)?, value).map_err(|e| format!("nie mogę zapisać ustawień: {e}"))
}

fn do_print(
    ip: IpAddr,
    path: &Path,
    copies: u32,
    color: bool,
    fit: bool,
) -> Result<PrintResultDto, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dokument")
        .to_string();
    let data = fs::read(path).map_err(|e| format!("nie mogę otworzyć „{name}”: {e}"))?;
    let mime = ipp::mime_for(path);
    let st = ipp::status(ip)?;
    let accepts = |m: &str| st.formats.is_empty() || st.formats.iter().any(|f| f == m);
    let opts = ipp::PrintOptions { copies: copies.max(1), color, fit };

    // 1. Format obsługiwany bezpośrednio
    if accepts(mime) && mime != "application/octet-stream" {
        let id = ipp::print(ip, &data, mime, &name, &opts)?;
        return Ok(PrintResultDto { jobs: vec![id], note: String::new() });
    }

    if !accepts("image/jpeg") {
        return Err(format!(
            "drukarka nie przyjmuje „{name}” ({mime}). Obsługiwane formaty: {}",
            st.formats.join(", ")
        ));
    }

    // 2. PDF → strony JPEG
    if mime == "application/pdf" {
        let (pages, dir) = pdf_to_jpegs(path)?;
        let mut ids = Vec::new();
        let single = ipp::PrintOptions { copies: 1, ..opts.clone() };
        let result = (|| {
            // Kopie po kolei (1,2,3…,1,2,3…), żeby strony były posortowane.
            for _ in 0..opts.copies {
                for (i, page) in pages.iter().enumerate() {
                    let title = format!("{name} – str. {}", i + 1);
                    ids.push(ipp::print(ip, page, "image/jpeg", &title, &single)?);
                }
            }
            Ok::<(), String>(())
        })();
        let _ = fs::remove_dir_all(dir);
        result?;
        return Ok(PrintResultDto {
            jobs: ids,
            note: format!("PDF zamieniony na {} stron(y) obrazu", pages.len()),
        });
    }

    // 3. Inny obraz → JPEG
    if mime.starts_with("image/") || mime == "application/octet-stream" {
        if let Ok(jpg) = image_to_jpeg(&data) {
            let id = ipp::print(ip, &jpg, "image/jpeg", &name, &opts)?;
            return Ok(PrintResultDto { jobs: vec![id], note: "obraz zamieniony na JPEG".into() });
        }
    }

    Err(format!(
        "nie umiem wydrukować „{name}”. Zapisz go jako PDF albo zdjęcie (JPG/PNG) i spróbuj ponownie."
    ))
}

// ---------------------------------------------------------------------
// Aktualizacje
// ---------------------------------------------------------------------

#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<updater::UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    blocking(move || updater::check(&current)).await
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<String, String> {
    let current = app.package_info().version.to_string();
    blocking(move || updater::install(&app, &current)).await
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle) -> Result<(), String> {
    updater::restart(&app)
}

#[tauri::command]
async fn driver_info() -> Result<DriverInfoDto, String> {
    blocking(|| {
        let installed = updater::installed_escpr();
        let latest = updater::latest_escpr();
        let update_available = match (&installed, &latest) {
            (Some(i), Some(l)) => updater::is_newer(l, i),
            _ => false,
        };
        Ok(DriverInfoDto { installed, latest, update_available, page: updater::ESCPR_PAGE.into() })
    })
    .await
}

/// Czy adres to panel WWW drukarki w sieci lokalnej (http://<IP>/)?
fn is_printer_panel(url: &str) -> bool {
    url.strip_prefix("http://")
        .and_then(|r| r.strip_suffix('/'))
        .and_then(|h| h.parse::<IpAddr>().ok())
        .map(|ip| match ip {
            IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
            IpAddr::V6(_) => false,
        })
        .unwrap_or(false)
}

/// Otwiera stronę w przeglądarce (GitHub, Epson albo panel drukarki).
#[tauri::command]
fn open_page(url: String) -> Result<(), String> {
    if !(url.starts_with("https://github.com/")
        || url.starts_with("https://support.epson.net/")
        || is_printer_panel(&url))
    {
        return Err("niedozwolony adres".into());
    }
    host_command("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("nie mogę otworzyć przeglądarki: {e}"))
}

// ---------------------------------------------------------------------

fn main() {
    // Na kartach NVIDIA WebKitGTK potrafi pokazać puste okno – to temu zapobiega.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            load_printer,
            save_printer,
            scan,
            status,
            jobs,
            cancel,
            print_file,
            setup_printer,
            maintenance,
            driver_info,
            photo_preview,
            printer_options,
            job_action,
            load_setting,
            save_setting,
            app_version,
            check_update,
            install_update,
            restart_app,
            open_page
        ])
        .run(tauri::generate_context!())
        .expect("nie udało się uruchomić aplikacji");
}
