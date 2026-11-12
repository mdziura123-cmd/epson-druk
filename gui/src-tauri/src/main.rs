//! Epson Druk – aplikacja z okienkiem (Tauri 2).

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
}

#[derive(Serialize)]
struct JobDto {
    id: i32,
    name: String,
    state: String,
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
async fn scan() -> Result<Vec<FoundDto>, String> {
    let found = blocking(ipp::scan).await?;
    Ok(found
        .into_iter()
        .map(|f| FoundDto { ip: f.ip.to_string(), model: f.model })
        .collect())
}

#[tauri::command]
async fn status(ip: String) -> Result<StatusDto, String> {
    let addr = parse_ip(&ip)?;
    let s = blocking(move || ipp::status(addr)).await?;
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
    })
}

#[tauri::command]
async fn jobs(ip: String) -> Result<Vec<JobDto>, String> {
    let addr = parse_ip(&ip)?;
    let j = blocking(move || ipp::jobs(addr)).await?;
    Ok(j
        .into_iter()
        .map(|j| JobDto { id: j.id, name: j.name, state: j.state })
        .collect())
}

#[tauri::command]
async fn cancel(ip: String, id: i32) -> Result<(), String> {
    let addr = parse_ip(&ip)?;
    blocking(move || ipp::cancel_job(addr, id)).await
}

#[tauri::command]
async fn print_file(
    ip: String,
    path: String,
    copies: u32,
    color: bool,
    fit: bool,
) -> Result<PrintResultDto, String> {
    let addr = parse_ip(&ip)?;
    blocking(move || do_print(addr, Path::new(&path), copies, color, fit)).await
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

/// Otwiera stronę wydania w przeglądarce (tylko adresy GitHuba).
#[tauri::command]
fn open_page(url: String) -> Result<(), String> {
    if !url.starts_with("https://github.com/") {
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
        .invoke_handler(tauri::generate_handler![
            load_printer,
            save_printer,
            scan,
            status,
            jobs,
            cancel,
            print_file,
            app_version,
            check_update,
            install_update,
            restart_app,
            open_page
        ])
        .run(tauri::generate_context!())
        .expect("nie udało się uruchomić aplikacji");
}
