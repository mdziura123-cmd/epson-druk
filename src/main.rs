//! epson-druk — prosty klient IPP dla drukarki Epson L3150 (i innych drukarek sieciowych).
//!
//! Bez zewnętrznych bibliotek: tylko biblioteka standardowa Rusta.
//!
//! Użycie:
//!   epson-druk szukaj                 – znajdź drukarki w sieci lokalnej (port 631)
//!   epson-druk status <IP>            – stan drukarki, tusz, obsługiwane formaty
//!   epson-druk drukuj <IP> <plik>     – wyślij plik (PDF / JPG / PWG / URF) do druku
//!   epson-druk zadania <IP>           – lista zadań w kolejce

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const IPP_PORT: u16 = 631;
const IPP_PATH: &str = "/ipp/print"; // ścieżka używana przez drukarki Epson

// ---------- Kody operacji IPP ----------
const OP_PRINT_JOB: u16 = 0x0002;
const OP_GET_JOBS: u16 = 0x000A;
const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;

// ---------- Znaczniki (tagi) IPP ----------
const TAG_OPERATION: u8 = 0x01;
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

    fn attr(&mut self, tag: u8, name: &str, value: &str) {
        self.raw(tag, name, value.as_bytes());
    }

    /// Kolejna wartość tego samego atrybutu (pusta nazwa).
    fn extra_value(&mut self, tag: u8, value: &str) {
        self.raw(tag, "", value.as_bytes());
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
            if i == 0 {
                self.attr(TAG_KEYWORD, name, v);
            } else {
                self.extra_value(TAG_KEYWORD, v);
            }
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
enum Val {
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
struct IppResponse {
    status: u16,
    /// (grupa, nazwa, wartości)
    attrs: Vec<(u8, String, Vec<Val>)>,
}

impl IppResponse {
    fn parse(data: &[u8]) -> Result<Self, String> {
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
                group = tag; // początek nowej grupy atrybutów
                continue;
            }
            let nlen = rd16(pos)?;
            pos += 2;
            let name = String::from_utf8_lossy(data.get(pos..pos + nlen).ok_or("zły rozmiar nazwy")?)
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

    fn get(&self, name: &str) -> Option<&Vec<Val>> {
        self.attrs.iter().find(|(_, n, _)| n == name).map(|(_, _, v)| v)
    }

    fn text(&self, name: &str) -> String {
        self.get(name)
            .map(|v| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "—".into())
    }

    fn ok(&self) -> bool {
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

    let head = format!(
        "POST {IPP_PATH} HTTP/1.1\r\nHost: {ip}:{IPP_PORT}\r\n\
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

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
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

fn printer_uri(ip: IpAddr) -> String {
    format!("ipp://{ip}:{IPP_PORT}{IPP_PATH}")
}

fn get_attributes(ip: IpAddr, timeout: Duration) -> Result<IppResponse, String> {
    let mut req = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, &printer_uri(ip));
    req.keywords(
        "requested-attributes",
        &[
            "printer-make-and-model",
            "printer-name",
            "printer-state",
            "printer-state-reasons",
            "printer-state-message",
            "marker-names",
            "marker-levels",
            "marker-colors",
            "document-format-supported",
            "printer-uuid",
        ],
    );
    ipp_send(ip, &req.finish(), timeout)
}

// =====================================================================
// Komendy
// =====================================================================

fn lokalny_ip() -> Option<Ipv4Addr> {
    // Sztuczka: "łączymy" gniazdo UDP (nic nie jest wysyłane), żeby poznać własny adres.
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

fn cmd_szukaj() -> Result<(), String> {
    let me = lokalny_ip().ok_or("nie mogę ustalić adresu IP komputera")?;
    let [a, b, c, _] = me.octets();
    println!("Szukam drukarek w sieci {a}.{b}.{c}.0/24 (port {IPP_PORT})…");

    let (tx, rx) = mpsc::channel();
    let mut watki = Vec::new();
    for chunk in (1u8..=254).collect::<Vec<_>>().chunks(32) {
        let chunk = chunk.to_vec();
        let tx = tx.clone();
        watki.push(thread::spawn(move || {
            for d in chunk {
                let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
                let addr = SocketAddr::new(ip, IPP_PORT);
                if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() {
                    tx.send(ip).ok();
                }
            }
        }));
    }
    drop(tx);
    for w in watki {
        w.join().ok();
    }
    let mut znalezione: Vec<IpAddr> = rx.iter().collect();
    znalezione.sort();

    if znalezione.is_empty() {
        println!("Nie znaleziono żadnej drukarki IPP. Sprawdź, czy drukarka jest włączona i w tej samej sieci.");
        return Ok(());
    }
    for ip in znalezione {
        match get_attributes(ip, Duration::from_secs(3)) {
            Ok(r) if r.ok() => println!("  ✔ {ip:<15}  {}", r.text("printer-make-and-model")),
            _ => println!("  ? {ip:<15}  (port 631 otwarty, ale brak odpowiedzi IPP)"),
        }
    }
    Ok(())
}

fn stan_slownie(v: Option<&Vec<Val>>) -> &'static str {
    match v.and_then(|v| v.first()) {
        Some(Val::Int(3)) => "gotowa",
        Some(Val::Int(4)) => "drukuje",
        Some(Val::Int(5)) => "zatrzymana",
        _ => "nieznany",
    }
}

fn cmd_status(ip: IpAddr) -> Result<(), String> {
    let r = get_attributes(ip, Duration::from_secs(5))?;
    if !r.ok() {
        return Err(format!("drukarka zwróciła błąd IPP 0x{:04x}", r.status));
    }
    println!("Drukarka : {}", r.text("printer-make-and-model"));
    println!("Adres    : {}", printer_uri(ip));
    println!("Stan     : {}", stan_slownie(r.get("printer-state")));
    println!("Powody   : {}", r.text("printer-state-reasons"));

    // Poziomy tuszu (o ile drukarka je raportuje)
    if let (Some(names), Some(levels)) = (r.get("marker-names"), r.get("marker-levels")) {
        println!("Tusz:");
        for (n, l) in names.iter().zip(levels.iter()) {
            let poziom = match l {
                Val::Int(p) if *p >= 0 => {
                    let p = (*p).min(100) as usize;
                    format!("[{}{}] {p:>3}%", "█".repeat(p / 5), "░".repeat(20 - p / 5))
                }
                _ => "nieznany (EcoTank – sprawdź okienka zbiorników)".into(),
            };
            println!("  {:<14} {poziom}", n.to_string());
        }
    } else {
        println!("Tusz     : drukarka nie raportuje poziomów (w L3150 widać je w okienkach zbiorników)");
    }
    println!("Formaty  : {}", r.text("document-format-supported"));
    Ok(())
}

fn format_pliku(path: &Path) -> &'static str {
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

fn cmd_drukuj(ip: IpAddr, plik: &str) -> Result<(), String> {
    let path = Path::new(plik);
    let dane = fs::read(path).map_err(|e| format!("nie mogę otworzyć {plik}: {e}"))?;
    let mime = format_pliku(path);

    // Sprawdź, czy drukarka przyjmie ten format
    let attrs = get_attributes(ip, Duration::from_secs(5))?;
    if let Some(formaty) = attrs.get("document-format-supported") {
        let lista: Vec<String> = formaty.iter().map(|f| f.to_string()).collect();
        if !lista.iter().any(|f| f == mime) {
            return Err(format!(
                "drukarka nie przyjmuje formatu {mime}.\n  Obsługiwane: {}\n  \
                 Wskazówka: przekonwertuj plik do jednego z nich (np. JPG) albo drukuj przez CUPS:\n  \
                 lp -d <nazwa_kolejki> {plik}",
                lista.join(", ")
            ));
        }
    }

    let nazwa = path.file_name().and_then(|n| n.to_str()).unwrap_or("dokument");
    let mut req = IppRequest::new(OP_PRINT_JOB, &printer_uri(ip));
    req.attr(TAG_NAME, "job-name", nazwa);
    req.attr(TAG_MIME, "document-format", mime);
    let mut body = req.finish();
    body.extend_from_slice(&dane);

    println!("Wysyłam „{nazwa}” ({} kB, {mime}) do {ip}…", dane.len() / 1024);
    let r = ipp_send(ip, &body, Duration::from_secs(5))?;
    if r.ok() {
        println!("✔ Zadanie przyjęte (job-id: {}). Drukarka zaraz zacznie drukować.", r.text("job-id"));
        Ok(())
    } else {
        Err(format!(
            "drukarka odrzuciła zadanie (kod IPP 0x{:04x}): {}",
            r.status,
            r.text("status-message")
        ))
    }
}

fn cmd_zadania(ip: IpAddr) -> Result<(), String> {
    let mut req = IppRequest::new(OP_GET_JOBS, &printer_uri(ip));
    req.keywords("requested-attributes", &["job-id", "job-name", "job-state"]);
    let r = ipp_send(ip, &req.finish(), Duration::from_secs(5))?;
    let ids: Vec<_> = r.attrs.iter().filter(|(_, n, _)| n == "job-id").collect();
    if ids.is_empty() {
        println!("Kolejka jest pusta.");
    }
    for (_, _, v) in ids {
        println!("  zadanie #{}", v[0]);
    }
    Ok(())
}

fn pomoc() {
    println!(
        "epson-druk — drukowanie bezprzewodowe na Epson L3150 (IPP)\n\n\
         Użycie:\n  \
         epson-druk szukaj               znajdź drukarki w sieci\n  \
         epson-druk status  <IP>         stan, tusz i obsługiwane formaty\n  \
         epson-druk drukuj  <IP> <plik>  wydrukuj plik\n  \
         epson-druk zadania <IP>         pokaż kolejkę zadań"
    );
}

fn parse_ip(s: Option<&String>) -> Result<IpAddr, String> {
    s.ok_or("podaj adres IP drukarki (znajdziesz go komendą: epson-druk szukaj)")?
        .parse()
        .map_err(|_| "niepoprawny adres IP".to_string())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let wynik = match args.get(1).map(|s| s.as_str()) {
        Some("szukaj") => cmd_szukaj(),
        Some("status") => parse_ip(args.get(2)).and_then(cmd_status),
        Some("zadania") => parse_ip(args.get(2)).and_then(cmd_zadania),
        Some("drukuj") => parse_ip(args.get(2)).and_then(|ip| match args.get(3) {
            Some(p) => cmd_drukuj(ip, p),
            None => Err("podaj plik do wydruku".into()),
        }),
        _ => {
            pomoc();
            Ok(())
        }
    };
    if let Err(e) = wynik {
        eprintln!("✘ Błąd: {e}");
        std::process::exit(1);
    }
}

// =====================================================================
// Testy
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn koduj_i_dekoduj() {
        let mut req = IppRequest::new(OP_GET_PRINTER_ATTRIBUTES, "ipp://10.0.0.5:631/ipp/print");
        req.keywords("requested-attributes", &["a", "b", "c"]);
        let bytes = req.finish();
        let r = IppResponse::parse(&bytes).unwrap();
        assert_eq!(r.text("printer-uri"), "ipp://10.0.0.5:631/ipp/print");
        assert_eq!(r.get("requested-attributes").unwrap().len(), 3);
    }

    #[test]
    fn chunked() {
        let d = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(d).unwrap(), b"Wikipedia");
    }
}
