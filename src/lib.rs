//! epson-druk — mała biblioteka IPP do drukowania bezprzewodowego (Epson L3150 i inne).
//!
//! Tylko biblioteka standardowa Rusta. Używana przez wersję terminalową i aplikację z okienkiem.

use std::env;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub const IPP_PORT: u16 = 631;
pub const IPP_PATH: &str = "/ipp/print"; // ścieżka używana przez drukarki Epson

// ---------- Kody operacji IPP ----------
const OP_PRINT_JOB: u16 = 0x0002;
const OP_CANCEL_JOB: u16 = 0x0008;
const OP_GET_JOBS: u16 = 0x000A;
const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;

// ---------- Znaczniki (tagi) IPP ----------
const TAG_OPERATION: u8 = 0x01;
const TAG_JOB: u8 = 0x02;
const TAG_END: u8 = 0x03;
const TAG_INTEGER: u8 = 0x21;
const TAG_BOOLEAN: u8 = 0x22;
const TAG_ENUM: u8 = 0x23;
const TAG_NAME: u8 = 0x42;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;
const TAG_MIME: u8 = 0x49;

// =====================================================================
// Typy publiczne
// =====================================================================

#[derive(Debug, Clone)]
pub struct Found {
    pub ip: IpAddr,
    /// `None` gdy port 631 jest otwarty, ale urządzenie nie odpowiedziało jak drukarka IPP.
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Ink {
    pub name: String,
    /// Procent 0–100, `None` gdy drukarka nie zna poziomu (typowe dla EcoTank).
    pub level: Option<u8>,
    /// Kolor w formacie `#RRGGBB`, jeśli drukarka go podaje.
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrinterStatus {
    pub model: String,
    pub uri: String,
    /// "gotowa", "drukuje", "zatrzymana" lub "nieznany".
    pub state: String,
    pub reasons: Vec<String>,
    pub message: String,
    pub inks: Vec<Ink>,
    pub formats: Vec<String>,
    pub color_supported: bool,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i32,
    pub name: String,
    /// "w kolejce", "wstrzymane", "drukuje", "zatrzymane", "anulowane", "przerwane", "gotowe".
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct PrintOptions {
    pub copies: u32,
    /// `true` = kolor, `false` = czarno-biało.
    pub color: bool,
    /// Dopasuj obraz do strony.
    pub fit: bool,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions { copies: 1, color: true, fit: true }
    }
}

// =====================================================================
// Kodowanie żądania IPP
// =====================================================================

struct IppRequest {
    buf: Vec<u8>,
}

impl IppRequest {
    fn new(op: u16, printer_uri: &str) -> Self {
        let mut r = IppRequest { buf: Vec::new() };
        r.buf.extend_from_slice(&[0x02, 0x00]); // wersja IPP 2.0
        r.buf.extend_from_slice(&op.to_be_bytes());
        r.buf.extend_from_slice(&1u32.to_be_bytes()); // request-id
        r.buf.push(TAG_OPERATION);
        r.attr(TAG_CHARSET, "attributes-charset", "utf-8");
        r.attr(TAG_LANGUAGE, "attributes-natural-language", "pl");
        r.attr(TAG_URI, "printer-uri", printer_uri);
        r.attr(TAG_NAME, "requesting-user-name", &uzytkownik());
        r
    }

    fn group(&mut self, tag: u8) {
        self.buf.push(tag);
    }

    fn attr(&mut self, tag: u8, name: &str, value: &str) {
        self.raw(tag, name, value.as_bytes());
    }

    fn int(&mut self, name: &str, value: i32) {
        self.raw(TAG_INTEGER, name, &value.to_be_bytes());
    }

    fn raw(&mut self, tag: u8, name: &str, value: &[u8]) {
        self.buf.push(tag);
        self.buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(value);
    }

    fn keywords(&mut self, name: &str, values: &[&str]) {
        for (i, v) in values.iter().enumerate() {
            let n = if i == 0 { name } else { "" };
            self.attr(TAG_KEYWORD, n, v);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.buf.push(TAG_END);
        self.buf
    }
}

fn uzytkownik() -> String {
    env::var("USER").unwrap_or_else(|_| "bazzite".into())
}

// =====================================================================
// Dekodowanie odpowiedzi IPP
// =====================================================================

#[derive(Debug, Clone)]
pub enum Val {
    Int(i32),
    Bool(bool),
    Text(String),
    Other(u8, Vec<u8>),
}

impl std::fmt::Display for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Val::Int(i) => write!(f, "{i}"),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Text(s) => write!(f, "{s}"),
            Val::Other(t, v) => write!(f, "<tag 0x{t:02x}, {} B>", v.len()),
        }
    }
}

#[derive(Debug, Default)]
pub struct IppResponse {
    pub status: u16,
    /// (grupa, nazwa, wartości)
    pub attrs: Vec<(u8, String, Vec<Val>)>,
}

impl IppResponse {
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 8 {
            return Err("odpowiedź IPP za krótka".into());
        }
        let mut resp = IppResponse {
            status: u16::from_be_bytes([data[2], data[3]]),
            attrs: Vec::new(),
        };
        let mut pos = 8;
        let mut group = 0u8;
        let rd16 = |p: usize| -> Result<usize, String> {
            data.get(p..p + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                .ok_or_else(|| "uszkodzona odpowiedź IPP".to_string())
        };
        while pos < data.len() {
            let tag = data[pos];
            pos += 1;
            if tag == TAG_END {
                break;
            }
            if tag < 0x10 {
                group = tag;
                continue;
            }
            let nlen = rd16(pos)?;
            pos += 2;
            let name = String::from_utf8_lossy(
                data.get(pos..pos + nlen).ok_or("zły rozmiar nazwy")?,
            )
            .to_string();
            pos += nlen;
            let vlen = rd16(pos)?;
            pos += 2;
            let raw = data.get(pos..pos + vlen).ok_or("zły rozmiar wartości")?;
            pos += vlen;

            let val = match tag {
                TAG_INTEGER | TAG_ENUM if vlen == 4 => {
                    Val::Int(i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
                }
                TAG_BOOLEAN if vlen == 1 => Val::Bool(raw[0] != 0),
                0x30..=0x33 | 0x35 | 0x36 | 0x41..=0x4A => {
                    Val::Text(String::from_utf8_lossy(raw).to_string())
                }
                _ => Val::Other(tag, raw.to_vec()),
            };

            if nlen == 0 {
                if let Some(last) = resp.attrs.last_mut() {
                    last.2.push(val);
                }
            } else {
                resp.attrs.push((group, name, vec![val]));
            }
        }
        Ok(resp)
    }

    pub fn get(&self, name: &str) -> Option<&Vec<Val>> {
        self.attrs.iter().find(|(_, n, _)| n == name).map(|(_, _, v)| v)
    }

    pub fn strings(&self, name: &str) -> Vec<String> {
        self.get(name)
            .map(|v| v.iter().map(|x| x.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn text(&self, name: &str) -> String {
        let s = self.strings(name);
        if s.is_empty() { "—".into() } else { s.join(", ") }
    }

    pub fn ok(&self) -> bool {
        self.status < 0x0100
    }
}

// =====================================================================
// HTTP (IPP jest przesyłane przez HTTP POST)
// =====================================================================

fn ipp_send(ip: IpAddr, body: &[u8], timeout: Duration) -> Result<IppResponse, String> {
    let addr = SocketAddr::new(ip, IPP_PORT);
    let mut s = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("nie mogę połączyć się z {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    s.set_write_timeout(Some(Duration::from_secs(60))).ok();

    let host = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    };
    let head = format!(
        "POST {IPP_PATH} HTTP/1.1\r\nHost: {host}:{IPP_PORT}\r\n\
         Content-Type: application/ipp\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;

    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| format!("błąd odczytu: {e}"))?;

    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("niepoprawna odpowiedź HTTP")?;
    let headers = String::from_utf8_lossy(&resp[..split]).to_lowercase();
    let mut body = resp[split + 4..].to_vec();

    let status_line = headers.lines().next().unwrap_or("");
    if !status_line.contains(" 200") {
        return Err(format!("drukarka odpowiedziała: {status_line}"));
    }
    if headers.contains("transfer-encoding: chunked") {
        body = dechunk(&body)?;
    }
    IppResponse::parse(&body)
}

pub fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("zły format chunked")?;
        let size_str = String::from_utf8_lossy(&data[..eol]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap().trim(), 16)
            .map_err(|_| "zły rozmiar chunka")?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(data.get(..size).ok_or("ucięty chunk")?);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
}

pub fn printer_uri(ip: IpAddr) -> String {
    let host = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    };
    format!("ipp://{host}:{IPP_PORT}{IPP_PATH}")
}

fn get_attributes(ip: IpAddr, timeout: Duration) -> Result<IppResponse, String> {
    let mut req = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, &printer_uri(ip));
    req.keywords(
        "requested-attributes",
        &[
            "printer-make-and-model",
            "printer-state",
            "printer-state-reasons",
            "printer-state-message",
            "marker-names",
            "marker-levels",
            "marker-colors",
            "document-format-supported",
            "color-supported",
        ],
    );
    ipp_send(ip, &req.finish(), timeout)
}

// =====================================================================
// Funkcje publiczne
// =====================================================================

pub fn local_ipv4() -> Option<Ipv4Addr> {
    // "Łączymy" gniazdo UDP (nic nie jest wysyłane), żeby poznać własny adres w sieci.
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

/// Przeszukuje sieć lokalną /24 w poszukiwaniu urządzeń z otwartym portem IPP.
pub fn scan() -> Result<Vec<Found>, String> {
    let me = local_ipv4().ok_or("nie mogę ustalić adresu IP komputera – czy jesteś w sieci?")?;
    let [a, b, c, _] = me.octets();

    let (tx, rx) = mpsc::channel();
    let mut watki = Vec::new();
    for chunk in (1u8..=254).collect::<Vec<_>>().chunks(16) {
        let chunk = chunk.to_vec();
        let tx = tx.clone();
        watki.push(thread::spawn(move || {
            for d in chunk {
                let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
                let addr = SocketAddr::new(ip, IPP_PORT);
                if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() {
                    let model = get_attributes(ip, Duration::from_secs(3))
                        .ok()
                        .filter(|r| r.ok())
                        .map(|r| r.text("printer-make-and-model"));
                    tx.send(Found { ip, model }).ok();
                }
            }
        }));
    }
    drop(tx);
    for w in watki {
        w.join().ok();
    }
    let mut out: Vec<Found> = rx.iter().collect();
    out.sort_by_key(|f| f.ip);
    Ok(out)
}

fn state_word(v: Option<&Vec<Val>>) -> &'static str {
    match v.and_then(|v| v.first()) {
        Some(Val::Int(3)) => "gotowa",
        Some(Val::Int(4)) => "drukuje",
        Some(Val::Int(5)) => "zatrzymana",
        _ => "nieznany",
    }
}

pub fn status(ip: IpAddr) -> Result<PrinterStatus, String> {
    let r = get_attributes(ip, Duration::from_secs(5))?;
    if !r.ok() {
        return Err(format!("drukarka zwróciła błąd IPP 0x{:04x}", r.status));
    }
    let names = r.strings("marker-names");
    let levels = r.get("marker-levels").cloned().unwrap_or_default();
    let colors = r.strings("marker-colors");
    let inks = names
        .iter()
        .enumerate()
        .map(|(i, n)| Ink {
            name: n.clone(),
            level: match levels.get(i) {
                Some(Val::Int(p)) if *p >= 0 => Some((*p).min(100) as u8),
                _ => None,
            },
            color: colors
                .get(i)
                .and_then(|c| c.split('#').nth(1))
                .map(|c| format!("#{}", &c[..c.len().min(6)])),
        })
        .collect();
    Ok(PrinterStatus {
        model: r.text("printer-make-and-model"),
        uri: printer_uri(ip),
        state: state_word(r.get("printer-state")).into(),
        reasons: r.strings("printer-state-reasons").into_iter().filter(|s| s != "none").collect(),
        message: r.strings("printer-state-message").join(" "),
        inks,
        formats: r.strings("document-format-supported"),
        color_supported: !matches!(
            r.get("color-supported").and_then(|v| v.first()),
            Some(Val::Bool(false))
        ),
    })
}

/// Typ MIME na podstawie rozszerzenia pliku.
pub fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("pwg") => "image/pwg-raster",
        Some("urf") => "image/urf",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Wysyła dane do druku. Zwraca numer zadania.
pub fn print(
    ip: IpAddr,
    data: &[u8],
    mime: &str,
    job_name: &str,
    opts: &PrintOptions,
) -> Result<i32, String> {
    let mut req = IppRequest::new(OP_PRINT_JOB, &printer_uri(ip));
    req.attr(TAG_NAME, "job-name", job_name);
    req.attr(TAG_MIME, "document-format", mime);
    req.group(TAG_JOB);
    req.int("copies", opts.copies.clamp(1, 99) as i32);
    req.attr(
        TAG_KEYWORD,
        "print-color-mode",
        if opts.color { "color" } else { "monochrome" },
    );
    if opts.fit {
        req.attr(TAG_KEYWORD, "print-scaling", "fit");
    }
    let mut body = req.finish();
    body.extend_from_slice(data);

    let r = ipp_send(ip, &body, Duration::from_secs(5))?;
    if r.ok() {
        Ok(match r.get("job-id").and_then(|v| v.first()) {
            Some(Val::Int(i)) => *i,
            _ => 0,
        })
    } else {
        Err(format!(
            "drukarka odrzuciła zadanie (kod IPP 0x{:04x}) {}",
            r.status,
            r.strings("status-message").join(" ")
        ))
    }
}

fn job_state_word(v: Option<&Val>) -> &'static str {
    match v {
        Some(Val::Int(3)) => "w kolejce",
        Some(Val::Int(4)) => "wstrzymane",
        Some(Val::Int(5)) => "drukuje",
        Some(Val::Int(6)) => "zatrzymane",
        Some(Val::Int(7)) => "anulowane",
        Some(Val::Int(8)) => "przerwane",
        Some(Val::Int(9)) => "gotowe",
        _ => "nieznany",
    }
}

pub fn jobs(ip: IpAddr) -> Result<Vec<Job>, String> {
    let mut req = IppRequest::new(OP_GET_JOBS, &printer_uri(ip));
    req.keywords("requested-attributes", &["job-id", "job-name", "job-state"]);
    let r = ipp_send(ip, &req.finish(), Duration::from_secs(5))?;
    let mut out = Vec::new();
    let mut cur: Option<Job> = None;
    for (group, name, vals) in &r.attrs {
        if *group != TAG_JOB {
            continue;
        }
        match name.as_str() {
            "job-id" => {
                if let Some(j) = cur.take() {
                    out.push(j);
                }
                let id = match vals.first() {
                    Some(Val::Int(i)) => *i,
                    _ => 0,
                };
                cur = Some(Job { id, name: String::new(), state: "nieznany".into() });
            }
            "job-name" => {
                if let Some(j) = cur.as_mut() {
                    j.name = vals.first().map(|v| v.to_string()).unwrap_or_default();
                }
            }
            "job-state" => {
                if let Some(j) = cur.as_mut() {
                    j.state = job_state_word(vals.first()).into();
                }
            }
            _ => {}
        }
    }
    if let Some(j) = cur {
        out.push(j);
    }
    Ok(out)
}

pub fn cancel_job(ip: IpAddr, job_id: i32) -> Result<(), String> {
    let mut req = IppRequest::new(OP_CANCEL_JOB, &printer_uri(ip));
    req.int("job-id", job_id);
    let r = ipp_send(ip, &req.finish(), Duration::from_secs(5))?;
    if r.ok() {
        Ok(())
    } else {
        Err(format!("nie udało się anulować zadania (kod IPP 0x{:04x})", r.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn koduj_i_dekoduj() {
        let mut req = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, "ipp://10.0.0.5:631/ipp/print");
        req.keywords("requested-attributes", &["a", "b", "c"]);
        req.group(TAG_JOB);
        req.int("copies", 2);
        let r = IppResponse::parse(&req.finish()).unwrap();
        assert_eq!(r.text("printer-uri"), "ipp://10.0.0.5:631/ipp/print");
        assert_eq!(r.get("requested-attributes").unwrap().len(), 3);
        assert!(matches!(r.get("copies").unwrap()[0], Val::Int(2)));
    }

    #[test]
    fn chunked() {
        let d = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(d).unwrap(), b"Wikipedia");
    }
}
