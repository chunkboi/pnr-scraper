use bytes::Bytes;
use colored::Colorize;
use image::{imageops::FilterType, DynamicImage, GrayImage, ImageBuffer, Luma};
use regex::Regex;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use std::simd::prelude::*;
use tesseract_plumbing::TessBaseApi;
use tokio::sync::{mpsc, oneshot};

const ENG_TRAINEDDATA: &[u8] = include_bytes!("../tessdata/eng.traineddata");

// =============================================================================
// Constants (matching Python exactly)
// =============================================================================

const CROP_X: u32 = 0;
const CROP_Y: u32 = 0;
const CROP_W: u32 = 80;
const CROP_H: u32 = 30;
const UPSCALE_FACTOR: u32 = 3;
const FAST_THRESHOLD: u8 = 128;
const FALLBACK_THRESHOLDS: &[u8] = &[110, 150];

static RE_CAPTCHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(-?\d+)\s*([+\-*])\s*(\d+)").unwrap());

/// Cached tessdata directory path — filesystem work happens only once per process.
static TESSDATA_PATH: OnceLock<PathBuf> = OnceLock::new();

pub struct OcrHandle {
    tx: Option<mpsc::Sender<OcrTask>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

struct OcrTask {
    img_bytes: Bytes,
    resp: oneshot::Sender<Option<i64>>,
}

impl OcrHandle {
    pub fn new(verbose: bool) -> Self {
        let (tx, mut rx) = mpsc::channel::<OcrTask>(32);

        // Spawn a dedicated OS thread for the C++ TessBaseApi to ensure RAII
        // and predictable destruction upon `main()` exit when `tx` drops.
        let worker = std::thread::spawn(move || {
            let tessdata_path = ensure_tessdata();
            let mut tess = TessBaseApi::create();
            let datapath_c = CString::new(tessdata_path.to_string_lossy().as_bytes())
                .expect("FATAL: Invalid datapath for Tesseract");
            let lang_c = CString::new("eng").unwrap();
            
            if tess.init_2(Some(&datapath_c), Some(&lang_c)).is_err() {
                panic!("FATAL: Tesseract failed to initialize. The eng.traineddata file at {:?} is missing or corrupted.", tessdata_path);
            }
            
            let whitelist = CString::new("tessedit_char_whitelist").unwrap();
            let chars = CString::new("0123456789+-").unwrap();
            let _ = tess.set_variable(&whitelist, &chars);
            let psm_name = CString::new("tessedit_pageseg_mode").unwrap();
            let psm_val = CString::new("7").unwrap();
            let _ = tess.set_variable(&psm_name, &psm_val);

            // Sync OS thread pulling from Tokio channel safely via blocking_recv
            while let Some(task) = rx.blocking_recv() {
                let res = solve_captcha_sync(&mut tess, &task.img_bytes, verbose);
                let _ = task.resp.send(res);
            }
        });

        Self {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    pub async fn solve(&self, img_bytes: Bytes) -> Option<i64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let task = OcrTask {
            img_bytes,
            resp: resp_tx,
        };
        // If the queue is full it yields back to tokio. If the receiver dropped, it returns gracefully.
        if let Some(tx) = &self.tx
            && tx.send(task).await.is_ok() {
                return resp_rx.await.unwrap_or(None);
            }
        None
    }
}

impl Drop for OcrHandle {
    fn drop(&mut self) {
        // Drop the transmitter explicitly which unblocks the rx.blocking_recv()
        self.tx.take();
        // We do NOT join() here. If Tesseract is hung in C++ FFI, join() would 
        // deadlock the entire runtime. The thread will terminate when the 
        // process exits.
        self.worker.take();
    }
}

// Character substitution map matching the Python _CHAR_MAP
fn char_map(c: char) -> char {
    match c {
        'O' | 'o' => '0',
        'l' | 'I' => '1',
        'B' => '8',
        'S' => '5',
        'Z' => '2',
        _ => c,
    }
}

fn apply_char_map(text: &str) -> String {
    text.chars().map(char_map).collect()
}

// =============================================================================
// Image Preprocessing (matching Python exactly)
// =============================================================================

fn mean_luminance(img: &GrayImage) -> f64 {
    let sum: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
    sum as f64 / (img.width() * img.height()) as f64
}

fn median_filter_3x3(img: &GrayImage) -> GrayImage {
    let (w, h) = img.dimensions();
    ImageBuffer::from_fn(w, h, |x, y| {
        let mut neighbours = [0u8; 9];
        let mut i = 0;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
                let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
                neighbours[i] = img.get_pixel(nx, ny).0[0];
                i += 1;
            }
        }
        neighbours.sort_unstable();
        Luma([neighbours[4]])
    })
}

fn enhance_contrast(img: &GrayImage, factor: f32) -> GrayImage {
    let mean: f32 =
        img.pixels().map(|p| p.0[0] as f32).sum::<f32>() / (img.width() * img.height()) as f32;
    ImageBuffer::from_fn(img.width(), img.height(), |x, y| {
        let v = img.get_pixel(x, y).0[0] as f32;
        let new_v = mean + (v - mean) * factor;
        Luma([new_v.clamp(0.0, 255.0) as u8])
    })
}

fn sharpen(img: &GrayImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let kernel: [i32; 9] = [0, -1, 0, -1, 5, -1, 0, -1, 0];
    ImageBuffer::from_fn(w, h, |x, y| {
        let mut sum = 0i32;
        let mut ki = 0;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
                let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
                sum += img.get_pixel(nx, ny).0[0] as i32 * kernel[ki];
                ki += 1;
            }
        }
        Luma([sum.clamp(0, 255) as u8])
    })
}

fn binarize(img: &GrayImage, threshold: u8) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    let pixels = img.as_raw();
    let out_pixels = out.as_mut();

    let threshold_simd = u8x32::splat(threshold);
    let zero_simd = u8x32::splat(0);
    let full_simd = u8x32::splat(255);

    // Image is 80x30 (upscaled x3 = 240x90). 240 * 90 = 21600 pixels.
    // 21600 is exactly divisible by 32 (675 iterations).
    // Manual SIMD processing replaces 21,600 branchy "if" statements
    // with 675 parallel register operations.
    let mut i = 0;
    let chunks = pixels.chunks_exact(32);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let simd_chunk = u8x32::from_slice(chunk);
        let mask = simd_chunk.simd_ge(threshold_simd);
        let result = mask.select(full_simd, zero_simd);
        result.copy_to_slice(&mut out_pixels[i..i + 32]);
        i += 32;
    }

    for (pixel, out_pixel) in remainder.iter().zip(&mut out_pixels[i..]) {
        *out_pixel = if *pixel >= threshold { 255 } else { 0 };
    }
    out
}

fn preprocess_base(img: &DynamicImage) -> GrayImage {
    let cropped = img.crop_imm(CROP_X, CROP_Y, CROP_W, CROP_H);

    // Correct alpha blending over a white background
    let mut rgba_image = cropped.into_rgba8();
    for pixel in rgba_image.pixels_mut() {
        let alpha = pixel.0[3] as u32;
        let r = pixel.0[0] as u32;
        let g = pixel.0[1] as u32;
        let b = pixel.0[2] as u32;

        let inv_alpha = 255 - alpha;

        // Composite over white (255, 255, 255)
        pixel.0[0] = ((r * alpha + 255 * inv_alpha + 127) / 255) as u8;
        pixel.0[1] = ((g * alpha + 255 * inv_alpha + 127) / 255) as u8;
        pixel.0[2] = ((b * alpha + 255 * inv_alpha + 127) / 255) as u8;
        pixel.0[3] = 255;
    }

    let mut gray = DynamicImage::ImageRgba8(rgba_image).into_luma8();

    if mean_luminance(&gray) < 128.0 {
        for pixel in gray.pixels_mut() {
            pixel.0[0] = 255 - pixel.0[0];
        }
    }

    let (w, h) = gray.dimensions();
    let upscaled = image::imageops::resize(
        &gray,
        w * UPSCALE_FACTOR,
        h * UPSCALE_FACTOR,
        FilterType::Triangle,
    );

    let filtered = median_filter_3x3(&upscaled);
    let contrasted = enhance_contrast(&filtered, 3.0);
    sharpen(&contrasted)
}

// =============================================================================
// Zero-copy OCR via tesseract-plumbing raw pixel API
// =============================================================================

fn ocr_in_memory(
    tess: &mut TessBaseApi,
    bin_img: &GrayImage,
    verbose: bool,
    label: &str,
) -> Option<i64> {
    let t = std::time::Instant::now();

    let (width, height) = bin_img.dimensions();
    let raw_pixels = bin_img.as_raw();

    // ZERO-COPY: feed raw grayscale pixels directly to Tesseract.
    // No PNG encode → decode round-trip. The GrayImage buffer is
    // contiguous 1-byte-per-pixel data with no padding, exactly what
    // TessBaseAPISetImage expects.
    if let Err(e) = tess.set_image(
        raw_pixels,
        width as i32,
        height as i32,
        1,                // bytes_per_pixel (grayscale)
        width as i32,     // bytes_per_line  (no row padding)
    ) {
        if verbose {
            eprintln!(
                "  {} {} {} tess set_image error: {:?}",
                "▸".bright_black(), "[OCR     ]".bright_cyan(), label, e
            );
        }
        return None;
    }

    // Set source resolution to suppress "Warning: Invalid resolution 0 dpi"
    tess.set_source_resolution(72);

    let raw = match tess.get_utf8_text() {
        Ok(text) => text.as_ref().to_string_lossy().trim().to_string(),
        Err(_) => {
            if verbose {
                eprintln!(
                    "  {} {} {} tess get_utf8_text error",
                    "▸".bright_black(), "[OCR     ]".bright_cyan(), label
                );
            }
            return None;
        }
    };
    let text = apply_char_map(&raw);
    let elapsed_ms = t.elapsed().as_millis();

    if let Some(caps) = RE_CAPTCHA.captures(&text) {
        let a: i64 = caps[1].parse().unwrap_or(0);
        let op = &caps[2];
        let b: i64 = caps[3].parse().unwrap_or(0);
        let result = match op {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            _ => return None,
        };
        if verbose {
            eprintln!(
                "  {} {} {} raw={:<20} parsed={}{}{}={} ✓  ({}ms)",
                "▸".bright_black(), "[OCR     ]".bright_cyan(),
                label,
                format!("{:?}", raw),
                a,
                op,
                b,
                result,
                elapsed_ms
            );
        }
        Some(result)
    } else {
        if verbose {
            eprintln!(
                "  {} {} {} raw={:<20} no match ✗  ({}ms)",
                "▸".bright_black(), "[OCR     ]".bright_cyan(),
                label,
                format!("{:?}", raw),
                elapsed_ms
            );
        }
        None
    }
}

fn solve_captcha_sync(tess: &mut TessBaseApi, img_bytes: &[u8], verbose: bool) -> Option<i64> {
    let t_start = std::time::Instant::now();

    let dyn_img = match image::load_from_memory(img_bytes) {
        Ok(i) => i,
        Err(e) => {
            if verbose {
                eprintln!(
                    "  {} {} Failed to decode captcha image: {}",
                    "▸".bright_black(), "[WARN    ]".bright_red(), e
                );
            }
            return None;
        }
    };

    let base = preprocess_base(&dyn_img);

    // Fast path: threshold=128, psm=7
    let fast_bin = binarize(&base, FAST_THRESHOLD);
    if let Some(r) = ocr_in_memory(tess, &fast_bin, verbose, "fast  thresh=128") {
        if verbose {
            eprintln!(
                "  {} {} Solved via fast path in {}ms",
                "▸".bright_black(), "[OCR     ]".bright_cyan(), t_start.elapsed().as_millis()
            );
        }
        return Some(r);
    }

    // Fallback sequential grid
    for &threshold in FALLBACK_THRESHOLDS {
        let bin = binarize(&base, threshold);
        let label = format!("grid  thresh={}", threshold);
        if let Some(res) = ocr_in_memory(tess, &bin, verbose, &label) {
            if verbose {
                eprintln!(
                    "  {} {} Solved via fallback grid in {}ms",
                    "▸".bright_black(), "[OCR     ]".bright_cyan(), t_start.elapsed().as_millis()
                );
            }
            return Some(res);
        }
    }

    if verbose {
        eprintln!(
            "  {} {} OCR completely failed ({}ms)",
            "▸".bright_black(), "[OCR     ]".bright_cyan(), t_start.elapsed().as_millis()
        );
    }
    None
}

/// Extracts the embedded eng.traineddata to a temp dir and returns that dir's path.
///
/// Tesseract 5.x interprets the `datapath` argument as the folder that directly
/// contains `<lang>.traineddata`, so we place `eng.traineddata` at the top level
/// of our temp directory (no `tessdata/` subdirectory).
///
/// The filesystem work is performed at most once per process via `OnceLock`.
fn ensure_tessdata() -> &'static PathBuf {
    TESSDATA_PATH.get_or_init(|| {
        let mut path = std::env::temp_dir();
        path.push("pnr-scraper-tessdata");

        if !path.exists()
            && let Err(e) = std::fs::create_dir_all(&path) {
                eprintln!(
                    "  {} {} Failed to create tessdata dir: {}",
                    "▸".bright_black(), "[OCR     ]".bright_cyan(), e
                );
            }

        let mut file_path = path.clone();
        file_path.push("eng.traineddata");

        let needs_write = !file_path.exists()
            || file_path.metadata().map(|m| m.len()).unwrap_or(0) != ENG_TRAINEDDATA.len() as u64;

        if needs_write {
            eprintln!(
                "  {} {} Extracting embedded eng.traineddata to {:?}...",
                "▸".bright_black(), "[OCR     ]".bright_cyan(), file_path
            );
            let mut tmp_path = path.clone();
            tmp_path.push(format!("eng.traineddata.{}.tmp", std::process::id()));
            
            if let Err(e) = std::fs::write(&tmp_path, ENG_TRAINEDDATA) {
                eprintln!(
                    "  {} {} Failed to write tessdata to temp file: {}",
                    "▸".bright_black(), "[OCR     ]".bright_cyan(), e
                );
            } else {
                // Atomic rename guarantees other processes won't read a half-written file.
                if let Err(e) = std::fs::rename(&tmp_path, &file_path) {
                    eprintln!(
                        "  {} {} Failed to atomically rename tessdata: {}",
                        "▸".bright_black(), "[OCR     ]".bright_cyan(), e
                    );
                }
            }
        }

        // Return the directory that directly contains eng.traineddata.
        // Tesseract 5.x resolves:  datapath + "/" + lang + ".traineddata"
        path
    })
}
