//! epson-druk — wersja terminalowa.
//!
//!   epson-druk szukaj                 – znajdź drukarki w sieci lokalnej
//!   epson-druk status <IP>            – stan drukarki, tusz, obsługiwane formaty
//!   epson-druk drukuj <IP> <plik>     – wyślij plik do druku
//!   epson-druk zadania <IP>           – lista zadań w kolejce

use epson_druk::*;
use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::Path;

fn cmd_szukaj() -> Result<(), String> {
    println!("Szukam drukarek w sieci lokalnej (port {IPP_PORT})…");
    let found = scan()?;
    if found.is_empty() {
        println!("Nie znaleziono drukarki. Sprawdź, czy jest włączona i w tej samej sieci Wi-Fi.");
    }
    for f in found {
        let how = match (f.ipp, f.raw) {
            (true, _) => "IPP – drukowanie bez sterownika",
            (false, true) => "port 9100 – drukowanie przez sterownik (CUPS)",
            _ => "",
        };
        println!("  ✔ {:<15}  {:<28} {how}", f.ip, f.model.unwrap_or_else(|| "nieznany model".into()));
    }
    Ok(())
}

fn cmd_status(ip: IpAddr) -> Result<(), String> {
    let s = status(ip)?;
    println!("Drukarka : {}", s.model);
    println!("Adres    : {}", s.uri);
    println!("Stan     : {}", s.state);
    if !s.reasons.is_empty() {
        println!("Uwagi    : {}", s.reasons.join(", "));
    }
    if s.inks.is_empty() {
        println!("Tusz     : drukarka nie raportuje poziomów (w L3150 widać je w okienkach zbiorników)");
    } else {
        println!("Tusz:");
        for ink in &s.inks {
            let poziom = match ink.level {
                Some(p) => {
                    let p = p as usize;
                    format!("[{}{}] {p:>3}%", "█".repeat(p / 5), "░".repeat(20 - p / 5))
                }
                None => "nieznany (EcoTank – sprawdź okienka zbiorników)".into(),
            };
            println!("  {:<14} {poziom}", ink.name);
        }
    }
    println!("Formaty  : {}", s.formats.join(", "));
    Ok(())
}

fn cmd_drukuj(ip: IpAddr, plik: &str) -> Result<(), String> {
    let path = Path::new(plik);
    let dane = fs::read(path).map_err(|e| format!("nie mogę otworzyć {plik}: {e}"))?;
    let mime = mime_for(path);
    let s = status(ip)?;
    if !s.formats.is_empty() && !s.formats.iter().any(|f| f == mime) {
        return Err(format!(
            "drukarka nie przyjmuje formatu {mime}.\n  Obsługiwane: {}\n  \
             Wskazówka: aplikacja z okienkiem sama konwertuje PDF i PNG.",
            s.formats.join(", ")
        ));
    }
    let nazwa = path.file_name().and_then(|n| n.to_str()).unwrap_or("dokument");
    println!("Wysyłam „{nazwa}” ({} kB, {mime}) do {ip}…", dane.len() / 1024);
    let id = print(ip, &dane, mime, nazwa, &PrintOptions::default())?;
    println!("✔ Zadanie przyjęte (nr {id}).");
    Ok(())
}

fn cmd_zadania(ip: IpAddr) -> Result<(), String> {
    let j = jobs(ip)?;
    if j.is_empty() {
        println!("Kolejka jest pusta.");
    }
    for job in j {
        println!("  #{:<5} {:<12} {}", job.id, job.state, job.name);
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
