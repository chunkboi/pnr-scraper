# 🚆 pnr-scraper — Indian Railways PNR Status Checker

A fast, fully self-contained Windows CLI tool that fetches live PNR status from
the Indian Railways enquiry portal. Built in Rust with statically linked
Tesseract OCR for captcha solving — the release binary has **no runtime
dependencies** beyond standard Windows system DLLs.

[![CodeQL](https://github.com/chunkboi/pnr-scraper/actions/workflows/codeql.yml/badge.svg)](https://github.com/chunkboi/pnr-scraper/actions/workflows/codeql.yml)
[![Dependabot](https://badgen.net/badge/Dependabot/enabled/green)](https://github.com/chunkboi/pnr-scraper)
[![DevSkim](https://github.com/chunkboi/pnr-scraper/actions/workflows/devskim.yml/badge.svg)](https://github.com/chunkboi/pnr-scraper/actions/workflows/devskim.yml)
[![CodeFactor](https://www.codefactor.io/repository/github/chunkboi/pnr-scraper/badge)](https://www.codefactor.io/repository/github/chunkboi/pnr-scraper)

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
  *(see portability note in Quick Start if you need to distribute the binary to
  other machines)*

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
> software installed, provided it was built without `target-cpu=native` (see
> portability note below).

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

> **⚠ Portability note:** The default `.cargo/config.toml` sets
> `-C target-cpu=native`, optimising for *your* CPU's instruction set
> (AVX2, SSE4.2, …). The resulting binary **will not run on older or different
> CPU models**. To produce a generic, portable binary, comment out the
> `rustflags` block in `.cargo/config.toml` before building:
> ```toml
> # [target.'cfg(target_arch = "x86_64")']
> # rustflags = ["-C", "target-cpu=native"]
> ```

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
captcha (e.g. `47 + 23`). On startup, the captcha-required check and the first
image download are issued **concurrently** (`tokio::join!`) so no sequential
round-trip is wasted. The solver then:

1. **Receives** the pre-fetched PNG captcha image
2. **Preprocesses** the image:
   - Alpha-composite over white background
   - Auto-invert if the image is dark-on-light
   - Upscale 3× with bilinear interpolation
   - Median filter (noise removal) → contrast enhancement → unsharp mask
3. **Binarizes** with a fast threshold (128) and tries OCR immediately
4. If the fast path fails, tries two fallback thresholds (110, 150) sequentially
5. **OCR** is performed by a single `OcrHandle` (a RAII wrapper around
   Tesseract 5) initialised once when `ApiClient` is constructed and shared
   across tasks via `Arc`. `eng.traineddata` is embedded in the binary at
   compile time and extracted atomically to a temp directory on first run.
6. **Evaluates** the parsed arithmetic expression and submits the answer
7. **Pipelining** — while the API request is in-flight, the next captcha image
   is fetched in the background (`fetch_captcha_owned`). On a retry this
   pre-fetched image is used immediately, saving one full network round-trip
   (~100–300 ms per retry).

### Session management

- On the first request a session is initialised by fetching the PNR enquiry
  page to obtain a `JSESSIONID` cookie
- The cookie is persisted to `~/.pnr_session.json` and reused for subsequent
  runs within the TTL window (default 5 minutes)
- If the server returns `"Session out or Invalid Request"`, the session is
  automatically purged and re-initialised
- Disk write failures (session cache, UA cache) are surfaced as visible
  `[WARN]` lines rather than silently discarded

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
│   ├── captcha.rs    # OcrHandle (RAII), image preprocessing, captcha solver
│   ├── ui.rs         # Terminal display, colour-coded output
│   └── models.rs     # Shared data types (PnrResult, SessionCache, …)
├── tessdata/              # ⚠ gitignored — created at build time
│   └── eng.traineddata   # Tesseract English model (embedded into binary at compile time)
├── .cargo/
│   └── config.toml   # VCPKGRS_TRIPLET + VCPKG_ROOT + target-cpu flag (relative path)
├── setup.ps1         # One-time vcpkg bootstrap script
├── Cargo.toml
└── Cargo.lock
```

---

## Building from source — details

### Why vcpkg?

`leptonica-sys` and `tesseract-sys` (the Rust FFI crates used by
`tesseract-plumbing`) use the
[vcpkg crate](https://crates.io/crates/vcpkg) to locate native libraries at
build time. `setup.ps1` installs those libraries with the
`x64-windows-static-md` triplet so they link statically into the final binary.

### `.cargo/config.toml`

```toml
[env]
VCPKGRS_TRIPLET = "x64-windows-static-md"
VCPKG_ROOT      = { value = "vcpkg_tools", relative = true }

# Enable native CPU instruction set (AVX2, SSE4.2, etc.) for auto-
# vectorisation of pixel processing loops. Only safe when you build
# and run on the same machine — comment this out for a portable binary.
[target.'cfg(target_arch = "x86_64")']
rustflags = ["-C", "target-cpu=native"]
```

`VCPKG_ROOT` uses Cargo's `relative = true` feature, so it resolves to
`<repo_root>/vcpkg_tools` regardless of where you clone the repository.

`target-cpu=native` enables AVX2/SSE4.2 auto-vectorisation for the
pixel-processing loops in the captcha pipeline. Remove or comment out that
block if you need the binary to run on machines other than the one it was built
on.

### Rebuilding after a `cargo clean`

```powershell
cargo build --release
```

vcpkg libraries are precompiled; only the Rust code is recompiled.

---

## License

MIT — see [LICENSE](LICENSE) for details.
