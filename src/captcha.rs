use image::{imageops::FilterType, DynamicImage, GrayImage, ImageBuffer, Luma};
use regex::Regex;
use std::cell::RefCell;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use tesseract_plumbing::TessBaseApi;

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

// Per-thread Tesseract instance: initialised once per thread, reused forever.
thread_local! {
    static TESS: RefCell<Option<TessBaseApi>> = const { RefCell::new(None) };
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
    ImageBuffer::from_fn(img.width(), img.height(), |x, y| {
        if img.get_pixel(x, y).0[0] < threshold {
            Luma([0])
        } else {
            Luma([255])
        }
    })
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
        pixel.0[0] = ((r * alpha + 255 * inv_alpha) / 255) as u8;
        pixel.0[1] = ((g * alpha + 255 * inv_alpha) / 255) as u8;
        pixel.0[2] = ((b * alpha + 255 * inv_alpha) / 255) as u8;
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

fn ocr_in_memory(bin_img: &GrayImage, _psm: i32, verbose: bool, label: &str) -> Option<i64> {
    let t = std::time::Instant::now();

    let (width, height) = bin_img.dimensions();
    let raw_pixels = bin_img.as_raw();

    // Retrieve the (already-written) tessdata dir — no filesystem work after first call.
    let tessdata_path = ensure_tessdata();

    // Access the per-thread TessBaseApi instance, initialising it exactly once per thread.
    let result = TESS.with(|cell| {
        let mut opt = cell.borrow_mut();

        if opt.is_none() {
            let mut api = TessBaseApi::create();
            let datapath_c = CString::new(tessdata_path.to_string_lossy().as_bytes()).ok()?;
            let lang_c = CString::new("eng").unwrap();
            if api.init_2(Some(&datapath_c), Some(&lang_c)).is_err() {
                if verbose {
                    eprintln!(
                        "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m {} tess init error",
                        label
                    );
                }
                return None;
            }
            let whitelist = CString::new("tessedit_char_whitelist").unwrap();
            let chars = CString::new("0123456789+-*").unwrap();
            let _ = api.set_variable(&whitelist, &chars);
            let psm_name = CString::new("tessedit_pageseg_mode").unwrap();
            let psm_val = CString::new("7").unwrap();
            let _ = api.set_variable(&psm_name, &psm_val);
            *opt = Some(api);
        }

        let tess = opt.as_mut().unwrap();

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
                    "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m {} tess set_image error: {:?}",
                    label, e
                );
            }
            return None;
        }

        // Set source resolution to suppress "Warning: Invalid resolution 0 dpi"
        tess.set_source_resolution(72);

        match tess.get_utf8_text() {
            Ok(text) => {
                let s = text.as_ref().to_string_lossy().trim().to_string();
                Some(s)
            }
            Err(_) => {
                if verbose {
                    eprintln!(
                        "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m {} tess get_utf8_text error",
                        label
                    );
                }
                None
            }
        }
    });

    let raw = result?;
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
                "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m {} raw={:<20} parsed={}{}{}={} ✓  ({}ms)",
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
                "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m {} raw={:<20} no match ✗  ({}ms)",
                label,
                format!("{:?}", raw),
                elapsed_ms
            );
        }
        None
    }
}

pub fn solve_captcha(img_bytes: &[u8], verbose: bool) -> Option<i64> {
    let t_start = std::time::Instant::now();

    let dyn_img = match image::load_from_memory(img_bytes) {
        Ok(i) => i,
        Err(e) => {
            if verbose {
                eprintln!(
                    "  \x1b[90m▸\x1b[0m \x1b[91m[WARN    ]\x1b[0m Failed to decode captcha image: {}",
                    e
                );
            }
            return None;
        }
    };

    let base = preprocess_base(&dyn_img);

    // Fast path: threshold=128, psm=7
    let fast_bin = binarize(&base, FAST_THRESHOLD);
    if let Some(r) = ocr_in_memory(&fast_bin, 7, verbose, "fast  thresh=128") {
        if verbose {
            eprintln!(
                "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m Solved via fast path in {}ms",
                t_start.elapsed().as_millis()
            );
        }
        return Some(r);
    }

    // Fallback sequential grid — thread-local Tess means no re-init cost per call.
    for &threshold in FALLBACK_THRESHOLDS {
        let bin = binarize(&base, threshold);
        let label = format!("grid  thresh={}", threshold);
        if let Some(res) = ocr_in_memory(&bin, 7, verbose, &label) {
            if verbose {
                eprintln!(
                    "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m Solved via fallback grid in {}ms",
                    t_start.elapsed().as_millis()
                );
            }
            return Some(res);
        }
    }

    if verbose {
        eprintln!(
            "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m OCR completely failed ({}ms)",
            t_start.elapsed().as_millis()
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
                    "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m Failed to create tessdata dir: {}",
                    e
                );
            }

        let mut file_path = path.clone();
        file_path.push("eng.traineddata");

        let needs_write = !file_path.exists()
            || file_path.metadata().map(|m| m.len()).unwrap_or(0) != ENG_TRAINEDDATA.len() as u64;

        if needs_write {
            eprintln!(
                "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m Extracting embedded eng.traineddata to {:?}...",
                file_path
            );
            if let Err(e) = std::fs::write(&file_path, ENG_TRAINEDDATA) {
                eprintln!(
                    "  \x1b[90m▸\x1b[0m \x1b[96m[OCR     ]\x1b[0m Failed to write tessdata: {}",
                    e
                );
            }
        }

        // Return the directory that directly contains eng.traineddata.
        // Tesseract 5.x resolves:  datapath + "/" + lang + ".traineddata"
        path
    })
}

/// Explicitly drop the thread-local Tesseract engine.
///
/// Tesseract keeps an internal process-wide `ObjectCache` (a static singleton).
/// If the thread-local `TessBaseApi` is still alive when that singleton's destructor
/// runs at process exit, Tesseract prints "WARNING! LEAK!" for every dawg object
/// it loaded.  Calling this function before returning from `main` ensures the
/// engine is torn down — and its reference counts released — while `ObjectCache`
/// is still intact.
pub fn cleanup() {
    TESS.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
