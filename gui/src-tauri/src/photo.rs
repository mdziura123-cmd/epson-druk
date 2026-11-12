//! Edycja zdjęć przed drukiem: obrót, kadrowanie do papieru, jasność, kontrast, nasycenie.

use image::{DynamicImage, ImageDecoder, ImageReader};
use serde::Deserialize;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn default_aspect() -> f32 {
    210.0 / 297.0
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, Clone, Debug)]
pub struct PhotoParams {
    /// 0, 90, 180, 270
    #[serde(default)]
    pub rotate: i32,
    /// -100..100
    #[serde(default)]
    pub brightness: i32,
    /// -100..100
    #[serde(default)]
    pub contrast: i32,
    /// -100..100
    #[serde(default)]
    pub saturation: i32,
    /// Wypełnij stronę (przytnij brzegi) zamiast dopasować z marginesem.
    #[serde(default)]
    pub fill: bool,
    /// Proporcje papieru (krótszy / dłuższy bok), np. A4 = 0.707.
    #[serde(default = "default_aspect")]
    pub aspect: f32,
    /// false = czarno-białe
    #[serde(default = "yes")]
    pub color: bool,
}

pub fn is_photo(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif")
    )
}

/// Wczytuje obraz z uwzględnieniem orientacji EXIF (zdjęcia z telefonu).
pub fn load(path: &Path) -> Result<DynamicImage, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| format!("nie mogę otworzyć zdjęcia: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("nie mogę odczytać zdjęcia: {e}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("nieobsługiwany format zdjęcia: {e}"))?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("uszkodzone zdjęcie: {e}"))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

pub fn process(img: DynamicImage, p: &PhotoParams) -> DynamicImage {
    let mut img = match p.rotate.rem_euclid(360) {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    };

    if p.fill && p.aspect > 0.0 {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let a = if p.aspect > 1.0 { 1.0 / p.aspect } else { p.aspect };
        // Papier obracamy tak jak zdjęcie (poziome zdjęcie → poziomy papier)
        let target = if w >= h { 1.0 / a } else { a };
        let (cw, ch) = if w / h > target { (h * target, h) } else { (w, w / target) };
        let x = ((w - cw) / 2.0).max(0.0) as u32;
        let y = ((h - ch) / 2.0).max(0.0) as u32;
        img = img.crop_imm(x, y, (cw as u32).max(1), (ch as u32).max(1));
    }

    if p.brightness != 0 {
        img = img.brighten((p.brightness.clamp(-100, 100) as f32 * 1.0) as i32);
    }
    if p.contrast != 0 {
        img = img.adjust_contrast(p.contrast.clamp(-100, 100) as f32 * 0.6);
    }

    let mut rgb = img.to_rgb8();
    if p.saturation != 0 || !p.color {
        let f = if p.color { 1.0 + p.saturation.clamp(-100, 100) as f32 / 100.0 } else { 0.0 };
        for px in rgb.pixels_mut() {
            let [r, g, b] = px.0;
            let l = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let c = |v: u8| (l + (v as f32 - l) * f).clamp(0.0, 255.0) as u8;
            px.0 = [c(r), c(g), c(b)];
        }
    }
    DynamicImage::ImageRgb8(rgb)
}

pub fn to_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let rgb = img.to_rgb8();
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| format!("nie mogę zapisać JPEG: {e}"))?;
    Ok(out.into_inner())
}

/// Pomniejszona kopia ostatnio edytowanego zdjęcia (szybki podgląd przy przesuwaniu suwaków).
static PREVIEW: Mutex<Option<(PathBuf, DynamicImage)>> = Mutex::new(None);

pub fn preview(path: &Path, p: &PhotoParams) -> Result<String, String> {
    let thumb = {
        let mut g = PREVIEW.lock().map_err(|_| "błąd wewnętrzny")?;
        match g.as_ref() {
            Some((cp, t)) if cp == path => t.clone(),
            _ => {
                let t = load(path)?.thumbnail(1100, 1100);
                *g = Some((path.to_path_buf(), t.clone()));
                t
            }
        }
    };
    let out = process(thumb, p);
    Ok(format!("data:image/jpeg;base64,{}", base64(&to_jpeg(&out, 82)?)))
}

/// Gotowy plik JPEG w pełnej rozdzielczości do druku.
pub fn render_for_print(path: &Path, p: &PhotoParams) -> Result<Vec<u8>, String> {
    let img = process(load(path)?, p);
    to_jpeg(&img, 95)
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn b64() {
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
