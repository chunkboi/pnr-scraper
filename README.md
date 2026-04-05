# 🚆 pnr-scraper — Indian Railways PNR Status Checker

A fast, fully self-contained Windows CLI tool that fetches live PNR status from
the Indian Railways enquiry portal. Built in Rust with statically linked
Tesseract OCR for captcha solving — the release binary has **no runtime
dependencies** beyond standard Windows system DLLs.

---

## Features

- **Live PNR lookup** — queries the official Indian Railways API directly
- **Automatic captcha solving** — preprocesses and solves the arithmetic image
  captcha using an embedded Tesseract 5 OCR engine (no external setup needed)
- **Session caching** — reuses the JSESSIONID cookie between runs to skip
  unnecessary session initialisation
- **Waitlist prediction** — automatically fetches CNF probability data when any
  passenger is on a waiting list
- **Rich terminal output** — colour-coded passenger status, fare, charting
  status, and a probability bar for WL predictions
- **JSON export** — optionally writes structured output to a `.json` file
- **Single binary** — everything (Tesseract model, OCR engine, TLS, HTTP
  client) is statically linked; copy `pnr-scraper.exe` anywhere and it works

---

## Requirements

| Requirement | Notes |
|---|---|
| Windows 10 / 11 (x64) | The only supported platform |
| [Rust + MSVC toolchain](https://rustup.rs) | `rustup` with the `x86_64-pc-windows-msvc` target |
| Visual Studio C++ tools | "Desktop development with C++" workload in VS Installer |
| Git | Needed by `setup.ps1` to clone vcpkg |

> **Note:** You only need Rust/MSVC/Git to *build* the project. The compiled
> `pnr-scraper.exe` binary runs on any 64-bit Windows machine with no extra
> software installed.

---

## Quick Start

### 1 — Clone the repo

```powershell
git clone https://github.com/chunkboi/pnr-scraper.git
cd pnr-scraper
```

### 2 — Bootstrap the native build dependencies

This clones [microsoft/vcpkg](https://github.com/microsoft/vcpkg), compiles it,
and then compiles Tesseract + Leptonica + all their image-format dependencies
as static libraries. **This only needs to be done once.** Expect 10–25 minutes
on first run.

```powershell
.\setup.ps1
```

> If you get a permissions error on `vcpkg integrate install`, re-run with
> `-SkipIntegrate` — the build will still work via the `VCPKG_ROOT` setting in
> `.cargo/config.toml`:
> ```powershell
> .\setup.ps1 -SkipIntegrate
> ```

### 3 — Build

```powershell
cargo build --release
```

The finished binary is at `target\release\pnr-scraper.exe`.

---

## Usage

```
pnr-scraper [OPTIONS]

Options:
  -p, --pnr <PNR>        10-digit PNR number
  -e, --export <FILE>    Export structured JSON to a file (e.g. data.json)
      --show-json        Print raw API JSON to the console
  -v, --verbose          Step-by-step debug logging with per-stage timings
      --ttl <SECONDS>    Local session TTL before force re-init [default: 900]
  -h, --help             Print help
  -V, --version          Print version
```

### Examples

```powershell
# Interactive — prompts for PNR
pnr-scraper.exe

# Pass PNR directly
pnr-scraper.exe --pnr 1234567890

# Export to JSON
pnr-scraper.exe --pnr 1234567890 --export status.json

# Show raw API response + verbose debug output
pnr-scraper.exe --pnr 1234567890 --show-json --verbose
```

### Sample output

```
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
                        🚆  PNR STATUS RESULT
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  │  PNR Number           1234567890
  │  As of                05-Apr-2026 [14:32:11 IST]
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  │  JOURNEY DETAILS
  │  ─────────────────────────────────────────────────────────────
  │  Train                12345 — RAJDHANI EXPRESS
  │  Date                 05-Apr-2026
  │  From → To            NDLS → MAS
  │  Class                1A
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  │  PASSENGER STATUS  (1 passenger)
  │  ─────────────────────────────────────────────────────────────
  │   Passenger 1  (GN)
  │     Booking :  CNF/B1/32
  │     Current :    ✓ CNF/B1/32
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  │  FARE & CHARTING
  │  ─────────────────────────────────────────────────────────────
  │  Total Fare           ₹ 4560
  │  Chart Status         Chart Prepared
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  ⏱  Fetched in 3.42 seconds
```

---

## How It Works

### Captcha solving pipeline

The Indian Railways enquiry portal protects its API with an arithmetic image
captcha (e.g. `47 + 23`). The solver:

1. **Fetches** the PNG captcha image over HTTPS
2. **Preprocesses** the image:
   - Alpha-composite over white background
   - Auto-invert if the image is dark-on-light
   - Upscale 3× with bilinear interpolation
   - Median filter (noise removal) → contrast enhancement → unsharp mask
3. **Binarizes** with a fast threshold (128) and tries OCR immediately
4. If the fast path fails, tries two fallback thresholds (110, 150) sequentially
5. **OCR** is performed by a thread-local Tesseract 5 instance (initialised once
   per thread; `eng.traineddata` is embedded in the binary and extracted to a
   temp directory on first run)
6. **Evaluates** the parsed arithmetic expression and submits the answer

### Session management

- On the first request a session is initialised by fetching the PNR enquiry
  page to obtain a `JSESSIONID` cookie
- The cookie is persisted to `~/.pnr_session.json` and reused for subsequent
  runs within the TTL window (default 5 minutes)
- If the server returns `"Session out or Invalid Request"`, the session is
  automatically purged and re-initialised

### Static binary

All native libraries (Tesseract 5, Leptonica, libjpeg, libpng, libtiff, zlib,
libwebp, libgif) are compiled with the vcpkg `x64-windows-static-md` triplet
and linked statically. The resulting `.exe` depends only on Windows system DLLs
(`KERNEL32`, `WS2_32`, `VCRUNTIME140`, etc.) that ship with every modern
Windows installation.

---

## Project Structure

```
pnr-scraper/
├── src/
│   ├── main.rs       # CLI argument parsing, top-level orchestration
│   ├── api.rs        # HTTP client, session management, fetch-retry loop
│   ├── captcha.rs    # Image preprocessing, OCR pipeline, captcha solver
│   ├── ui.rs         # Terminal display, colour-coded output
│   └── models.rs     # Shared data types (PnrResult, SessionCache, …)
├── tessdata/
│   └── eng.traineddata   # Tesseract English model (embedded into binary)
├── .cargo/
│   └── config.toml   # VCPKGRS_TRIPLET + VCPKG_ROOT (relative path)
├── setup.ps1         # One-time vcpkg bootstrap script
├── Cargo.toml
└── Cargo.lock
```

---

## Building from source — details

### Why vcpkg?

`leptonica-sys` and `tesseract-sys` (the Rust FFI crates) use the
[vcpkg crate](https://crates.io/crates/vcpkg) to locate native libraries at
build time. `setup.ps1` installs those libraries with the
`x64-windows-static-md` triplet so they link statically into the final binary.

### `.cargo/config.toml`

```toml
[env]
VCPKGRS_TRIPLET = "x64-windows-static-md"
VCPKG_ROOT      = { value = "vcpkg_tools", relative = true }
```

`VCPKG_ROOT` uses Cargo's `relative = true` feature, so it resolves to
`<repo_root>/vcpkg_tools` regardless of where you clone the repository.

### Rebuilding after a `cargo clean`

```powershell
cargo build --release
```

vcpkg libraries are precompiled; only the Rust code is recompiled.

---

## License

MIT — see [LICENSE](LICENSE) for details.
