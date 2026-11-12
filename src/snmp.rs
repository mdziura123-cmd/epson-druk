//! Minimalny klient SNMP v1 (tylko GET) oraz odczyt stanu drukarek Epson.
//!
//! Źródła stanu (w kolejności):
//! 1. Epson „@BDC ST2” (OID prywatny Epsona) – stan, błędy (brak papieru…), poziom tuszu.
//! 2. Standardowy Printer-MIB / Host-Resources-MIB – jako zapas.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

pub const COMMUNITY: &str = "public";

/// Stan Epson ST2 (ogólne zapytanie).
const OID_EPSON_STATUS: &str = "1.3.6.1.4.1.1248.1.2.2.1.1.1.4.1";
/// Polecenie EPSON-CTRL "st" (zapas, gdy powyższy OID nie odpowiada).
const OID_EPSON_CTRL_ST: &str = "1.3.6.1.4.1.1248.1.2.2.44.1.1.2.1.115.116.1.0.1";
/// Polecenie EPSON-CTRL "vi" – wersja oprogramowania drukarki.
const OID_EPSON_CTRL_VI: &str = "1.3.6.1.4.1.1248.1.2.2.44.1.1.2.1.118.105.1.0.0";
/// hrPrinterDetectedErrorState
const OID_HR_ERROR_STATE: &str = "1.3.6.1.2.1.25.3.5.1.2.1";

#[derive(Debug, Clone, PartialEq)]
pub enum SnmpValue {
    Int(i64),
    Bytes(Vec<u8>),
    Null,
    Other(u8, Vec<u8>),
}

// ---------------------------------------------------------------- BER

fn ber_len(n: usize, out: &mut Vec<u8>) {
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    ber_len(content.len(), &mut v);
    v.extend_from_slice(content);
    v
}

fn ber_int(n: i64) -> Vec<u8> {
    let mut b = n.to_be_bytes().to_vec();
    while b.len() > 1 && ((b[0] == 0 && b[1] & 0x80 == 0) || (b[0] == 0xff && b[1] & 0x80 != 0)) {
        b.remove(0);
    }
    tlv(0x02, &b)
}

fn ber_oid(oid: &str) -> Result<Vec<u8>, String> {
    let parts: Vec<u64> = oid
        .split('.')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u64>().map_err(|_| format!("zły OID {oid}")))
        .collect::<Result<_, _>>()?;
    if parts.len() < 2 {
        return Err(format!("zły OID {oid}"));
    }
    let mut c = vec![(parts[0] * 40 + parts[1]) as u8];
    for &p in &parts[2..] {
        let mut stack = vec![(p & 0x7f) as u8];
        let mut v = p >> 7;
        while v > 0 {
            stack.push(((v & 0x7f) as u8) | 0x80);
            v >>= 7;
        }
        stack.reverse();
        c.extend(stack);
    }
    Ok(tlv(0x06, &c))
}

pub fn build_get(oid: &str, request_id: i64) -> Result<Vec<u8>, String> {
    let varbind = tlv(0x30, &[ber_oid(oid)?, vec![0x05, 0x00]].concat());
    let varbinds = tlv(0x30, &varbind);
    let pdu = tlv(0xA0, &[ber_int(request_id), ber_int(0), ber_int(0), varbinds].concat());
    Ok(tlv(0x30, &[ber_int(0), tlv(0x04, COMMUNITY.as_bytes()), pdu].concat()))
}

/// Czyta jeden element TLV: (tag, zawartość, reszta).
fn read_tlv(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *d.first()?;
    let first = *d.get(1)? as usize;
    let (len, hdr) = if first & 0x80 == 0 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut l = 0usize;
        for i in 0..n {
            l = (l << 8) | *d.get(2 + i)? as usize;
        }
        (l, 2 + n)
    };
    let content = d.get(hdr..hdr + len)?;
    Some((tag, content, &d[hdr + len..]))
}

fn int_of(c: &[u8]) -> i64 {
    let mut v: i64 = if c.first().map(|b| b & 0x80 != 0).unwrap_or(false) { -1 } else { 0 };
    for b in c {
        v = (v << 8) | *b as i64;
    }
    v
}

/// Zwraca (request-id, error-status, wartość pierwszego varbindu).
pub fn parse_response(d: &[u8]) -> Option<(i64, i64, SnmpValue)> {
    let (_, msg, _) = read_tlv(d)?; // SEQUENCE
    let (_, _ver, rest) = read_tlv(msg)?;
    let (_, _comm, rest) = read_tlv(rest)?;
    let (pdu_tag, pdu, _) = read_tlv(rest)?;
    if pdu_tag != 0xA2 {
        return None;
    }
    let (_, reqid, rest) = read_tlv(pdu)?;
    let (_, err, rest) = read_tlv(rest)?;
    let (_, _idx, rest) = read_tlv(rest)?;
    let (_, vbs, _) = read_tlv(rest)?;
    let (_, vb, _) = read_tlv(vbs)?;
    let (_, _oid, rest) = read_tlv(vb)?;
    let (vtag, val, _) = read_tlv(rest)?;
    let value = match vtag {
        0x02 | 0x41 | 0x42 | 0x43 => SnmpValue::Int(int_of(val)),
        0x04 => SnmpValue::Bytes(val.to_vec()),
        0x05 => SnmpValue::Null,
        t => SnmpValue::Other(t, val.to_vec()),
    };
    Some((int_of(reqid), int_of(err), value))
}

/// SNMP GET jednego OID. `None` gdy brak odpowiedzi lub OID nieobsługiwany.
pub fn get(ip: IpAddr, oid: &str) -> Option<SnmpValue> {
    let sock = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    let reqid = (std::process::id() as i64 & 0x7fff) * 1000 + (oid.len() as i64);
    let pkt = build_get(oid, reqid).ok()?;
    let target = SocketAddr::new(ip, 161);
    let mut buf = vec![0u8; 8192];
    for _ in 0..2 {
        sock.send_to(&pkt, target).ok()?;
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            if from.ip() != ip {
                continue;
            }
            if let Some((rid, err, val)) = parse_response(&buf[..n]) {
                if rid != reqid {
                    continue;
                }
                if err != 0 {
                    return None;
                }
                return match val {
                    SnmpValue::Other(t, _) if (0x80..=0x82).contains(&t) => None, // noSuchObject…
                    v => Some(v),
                };
            }
        }
    }
    None
}

// ---------------------------------------------------------------- Epson ST2

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpsonStatus {
    /// Kod stanu (pole 0x01).
    pub status: Option<u8>,
    /// Kod błędu (pole 0x02).
    pub error: Option<u8>,
    /// Kody ostrzeżeń (pole 0x04).
    pub warnings: Vec<u8>,
    /// (kolor, procent) – kolor: 0 czarny, 1 błękitny, 2 purpurowy, 3 żółty…
    pub inks: Vec<(u8, u8)>,
}

/// Parsuje odpowiedź „@BDC ST2\r\n” + długość (2 B LE) + pola [kod][długość][dane].
pub fn parse_st2(data: &[u8]) -> Option<EpsonStatus> {
    let start = data.windows(8).position(|w| w == b"@BDC ST2")?;
    let mut p = start + 8;
    while p < data.len() && (data[p] == b'\r' || data[p] == b'\n') {
        p += 1;
    }
    let len = u16::from_le_bytes([*data.get(p)?, *data.get(p + 1)?]) as usize;
    p += 2;
    let end = (p + len).min(data.len());
    let mut st = EpsonStatus::default();
    while p + 2 <= end {
        let code = data[p];
        let flen = data[p + 1] as usize;
        let f = data.get(p + 2..(p + 2 + flen).min(end)).unwrap_or(&[]);
        match code {
            0x01 if !f.is_empty() => st.status = Some(f[0]),
            0x02 if !f.is_empty() => st.error = Some(f[0]),
            0x04 => st.warnings = f.to_vec(),
            0x0f if !f.is_empty() => {
                let esz = f[0] as usize;
                if esz >= 3 {
                    for e in f[1..].chunks(esz) {
                        if e.len() >= 3 {
                            st.inks.push((e[1], e[2].min(100)));
                        }
                    }
                }
            }
            _ => {}
        }
        p += 2 + flen;
    }
    Some(st)
}

pub fn ink_color_name(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("Black", "#222831"),
        1 => ("Cyan", "#00a3e0"),
        2 => ("Magenta", "#d4007f"),
        3 => ("Yellow", "#f2c500"),
        4 => ("Light Cyan", "#7fd3f7"),
        5 => ("Light Magenta", "#f59bd0"),
        9 => ("Dark Yellow", "#c9a400"),
        0x0a => ("Light Black", "#6b7280"),
        0x0b => ("Matte Black", "#111111"),
        0x0f => ("Light Light Black", "#9aa0a6"),
        _ => ("Ink", "#7f8c9d"),
    }
}

// ---------------------------------------------------------------- wynik ogólny

#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    /// Słowny stan, np. "gotowa", "drukuje", "czyszczenie", "błąd".
    pub state: Option<String>,
    /// Słowa kluczowe zgodne z IPP: media-empty, media-jam, cover-open, marker-supply-empty, marker-supply-low…
    pub reasons: Vec<String>,
    /// (nazwa, procent, kolor #RRGGBB)
    pub inks: Vec<(String, u8, String)>,
    /// Skąd dane: "epson" / "mib".
    pub source: Option<String>,
}

fn epson_state(st: &EpsonStatus) -> DeviceState {
    let mut d = DeviceState { source: Some("epson".into()), ..Default::default() };
    d.state = st.status.map(|s| {
        match s {
            0 => "błąd",
            1 => "drukuje stronę testową",
            2 => "zajęta",
            3 => "czeka",
            4 => "gotowa",
            5 => "wstrzymana",
            7 => "czyszczenie",
            8 => "tryb fabryczny",
            10 => "wyłącza się",
            _ => "nieznany",
        }
        .to_string()
    });
    if let Some(e) = st.error {
        // Kody błędów Epson (wg escputil/epson_print_conf)
        let r = match e {
            0 => Some("fatal-error"),
            2 => Some("cover-open"),
            4 => Some("media-jam"),
            5 => Some("marker-supply-empty"),
            6 => Some("media-empty"),
            0x0c => Some("media-jam"),
            0x10 => Some("waste-ink-full"),
            _ => None,
        };
        // Przy stanie „błąd” uznajemy każdy kod; w innych stanach tylko jednoznaczne
        // (papier, pokrywa, zacięcie), żeby nie pokazywać fałszywych alarmów.
        let clear_kind = matches!(e, 2 | 4 | 6 | 0x0c);
        if let Some(r) = r {
            if st.status == Some(0) || clear_kind {
                d.reasons.push(r.into());
            }
        }
    }
    for (code, pct) in &st.inks {
        let (name, color) = ink_color_name(*code);
        d.inks.push((name.into(), *pct, color.into()));
        if *pct == 0 && !d.reasons.iter().any(|r| r == "marker-supply-empty") {
            d.reasons.push("marker-supply-empty".into());
        } else if *pct <= 10 && !d.reasons.iter().any(|r| r.starts_with("marker-supply")) {
            d.reasons.push("marker-supply-low".into());
        }
    }
    d
}

/// Odczyt stanu standardowym Printer-MIB (gdy Epson ST2 nie działa).
fn mib_state(ip: IpAddr) -> Option<DeviceState> {
    let mut d = DeviceState { source: Some("mib".into()), ..Default::default() };
    let mut any = false;
    if let Some(SnmpValue::Bytes(b)) = get(ip, OID_HR_ERROR_STATE) {
        any = true;
        let b0 = b.first().copied().unwrap_or(0);
        if b0 & 0x40 != 0 { d.reasons.push("media-empty".into()); }
        if b0 & 0x80 != 0 { d.reasons.push("media-low".into()); }
        if b0 & 0x04 != 0 { d.reasons.push("media-jam".into()); }
        if b0 & 0x08 != 0 { d.reasons.push("cover-open".into()); }
        if b0 & 0x10 != 0 { d.reasons.push("marker-supply-empty".into()); }
        if b0 & 0x20 != 0 { d.reasons.push("marker-supply-low".into()); }
    }
    for i in 1..=6 {
        let level = match get(ip, &format!("1.3.6.1.2.1.43.11.1.1.9.1.{i}")) {
            Some(SnmpValue::Int(v)) => v,
            _ => break,
        };
        any = true;
        let max = match get(ip, &format!("1.3.6.1.2.1.43.11.1.1.8.1.{i}")) {
            Some(SnmpValue::Int(v)) if v > 0 => v,
            _ => 100,
        };
        let name = match get(ip, &format!("1.3.6.1.2.1.43.11.1.1.6.1.{i}")) {
            Some(SnmpValue::Bytes(b)) => String::from_utf8_lossy(&b).trim().to_string(),
            _ => format!("Tusz {i}"),
        };
        if level >= 0 {
            let pct = ((level * 100) / max).clamp(0, 100) as u8;
            let lname = name.to_lowercase();
            let code = if lname.contains("black") { 0 } else if lname.contains("cyan") { 1 }
                else if lname.contains("magenta") { 2 } else if lname.contains("yellow") { 3 } else { 99 };
            d.inks.push((name, pct, ink_color_name(code).1.into()));
        }
    }
    any.then_some(d)
}

/// Stan urządzenia przez SNMP. `None` gdy drukarka nie odpowiada na SNMP.
pub fn device_state(ip: IpAddr) -> Option<DeviceState> {
    for oid in [OID_EPSON_STATUS, OID_EPSON_CTRL_ST] {
        if let Some(SnmpValue::Bytes(b)) = get(ip, oid) {
            if let Some(st) = parse_st2(&b) {
                return Some(epson_state(&st));
            }
        }
    }
    mib_state(ip)
}

/// Wersja oprogramowania (firmware) drukarki Epson, np. "LF23K8".
pub fn firmware_version(ip: IpAddr) -> Option<String> {
    let SnmpValue::Bytes(b) = get(ip, OID_EPSON_CTRL_VI)? else { return None };
    let text = String::from_utf8_lossy(&b).to_string();
    let i = text.find("vi:")?;
    // Format: "vi:00:XXXXXX…;" – bierzemy pierwsze pole po kodzie
    let rest = &text[i + 3..];
    let rest = rest.split_once(':').map(|(_, r)| r).unwrap_or(rest);
    let ver: String = rest
        .chars()
        .take_while(|c| *c != ';' && *c != ',' && !c.is_control())
        .collect();
    let ver = ver.trim().to_string();
    (!ver.is_empty()).then_some(ver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_kodowanie() {
        assert_eq!(ber_oid("1.3.6.1.4.1.1248").unwrap(), vec![0x06, 0x07, 0x2b, 6, 1, 4, 1, 0x89, 0x60]);
    }

    #[test]
    fn st2() {
        let mut d = b"@BDC ST2\r\n".to_vec();
        let body: Vec<u8> = vec![
            0x01, 0x01, 0x04, // stan: gotowa
            0x02, 0x01, 0x06, // błąd: brak papieru
            0x0f, 0x0d, 0x03, 0x01, 0x00, 0x4c, 0x03, 0x01, 0x48, 0x04, 0x02, 0x05, 0x05, 0x03, 0x45,
        ];
        d.extend_from_slice(&(body.len() as u16).to_le_bytes());
        d.extend(body);
        let st = parse_st2(&d).unwrap();
        assert_eq!(st.status, Some(4));
        assert_eq!(st.error, Some(6));
        assert_eq!(st.inks, vec![(0, 76), (1, 72), (2, 5), (3, 69)]);
        let ds = epson_state(&st);
        assert!(ds.reasons.contains(&"media-empty".to_string()));
        assert!(ds.reasons.contains(&"marker-supply-low".to_string()));
        assert_eq!(ds.state.as_deref(), Some("gotowa"));
    }
}
