use crate::captcha;
use crate::models::{
    CookieEntry, FareInfo, JourneyInfo, MappedPassenger, MappedResponse, PnrResult, SessionCache,
    UaCache,
};
use bytes::Bytes;
use colored::Colorize;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, ORIGIN, REFERER,
    SET_COOKIE, USER_AGENT,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const PNR_PAGE: &str = "https://www.indianrail.gov.in/enquiry/PNR/PnrEnquiry.html?locale=en";
const CAPTCHA_URL: &str = "https://www.indianrail.gov.in/enquiry/captchaDraw.png";
const CAPTCHA_CONFIG_URL: &str = "https://www.indianrail.gov.in/enquiry/CaptchaConfig";
const API_URL: &str = "https://www.indianrail.gov.in/enquiry/CommonCaptcha";
const MAX_CAPTCHA_RETRIES: u32 = 6;
const SESSION_TTL: f64 = 300.0;
const UA_CACHE_TTL: f64 = 86400.0;

/// Parsed once at first use; every `add_cookies_to_jar` call reuses this.
static BASE_URL: LazyLock<reqwest::Url> =
    LazyLock::new(|| "https://www.indianrail.gov.in".parse().unwrap());

// =============================================================================
// Small helpers
// =============================================================================

fn now_sec() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn cache_dir() -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join("pnr-scraper")
}

fn ensure_cache_dir() {
    let dir = cache_dir();
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
}

fn session_file() -> PathBuf {
    cache_dir().join("session.json")
}

fn ua_file() -> PathBuf {
    cache_dir().join("ua_cache.json")
}

/// Insert every entry from `cookies` into `jar` without rebuilding the client.
fn add_cookies_to_jar(jar: &reqwest::cookie::Jar, cookies: &HashMap<String, String>) {
    for (k, v) in cookies {
        jar.add_cookie_str(&format!("{}={}", k, v), &BASE_URL);
    }
}

async fn get_latest_user_agent() -> String {
    // Keep as &str — only allocate a String when we actually need one.
    const FALLBACK: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";

    let path = ua_file();
    if let Ok(data) = std::fs::read_to_string(&path)
        && let Ok(cache) = serde_json::from_str::<UaCache>(&data)
            && now_sec() - cache.ts < UA_CACHE_TTL {
                return cache.ua;
            }
    let ua = FALLBACK.to_string();
    let cache = UaCache {
        ua: ua.clone(),
        ts: now_sec(),
    };
    ensure_cache_dir();
    if let Err(e) = std::fs::write(&path, serde_json::to_string(&cache).unwrap()) {
        eprintln!(
            "  {} {} Failed to cache User-Agent to disk: {}",
            "▸".bright_black(), "[WARN    ]".bright_red(), e
        );
    }
    ua
}

// =============================================================================
// State
// =============================================================================

struct AppState {
    /// Shared cookie jar. Kept alive independently from `client` so that new
    /// cookies can be injected without discarding the connection pool.
    jar: Arc<reqwest::cookie::Jar>,
    /// The HTTP client. Built once per session; never rebuilt for cookie updates.
    client: Option<reqwest::Client>,
    session_ts: f64,
    ua: String,
    /// Mirrors what is in `jar`; used only for disk serialisation.
    cookies: HashMap<String, String>,
    /// One-time DNS resolution result to bypass redundant lookups.
    resolved_ip: Option<SocketAddr>,
}

pub struct ApiClient {
    state: Arc<Mutex<AppState>>,
    ocr: Arc<captcha::OcrHandle>,
    verbose: bool,
}

// =============================================================================
// ApiClient implementation
// =============================================================================

impl ApiClient {
    pub async fn new(verbose: bool) -> Self {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let state = AppState {
            jar,
            client: None,
            session_ts: 0.0,
            ua: get_latest_user_agent().await,
            cookies: HashMap::new(),
            resolved_ip: None,
        };

        Self {
            state: Arc::new(Mutex::new(state)),
            ocr: Arc::new(captcha::OcrHandle::new(verbose)),
            verbose,
        }
    }

    async fn refresh_dns(&self) -> Option<SocketAddr> {
        self.dbg("SESSION", "Refreshing DNS for www.indianrail.gov.in...", None);
        match tokio::net::lookup_host("www.indianrail.gov.in:443").await {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    self.dbg("SESSION", &format!("DNS Resolved: {}", addr), None);
                    return Some(addr);
                }
            }
            Err(e) => {
                self.dbg("WARN", &format!("DNS resolution failed: {}", e), None);
            }
        }
        None
    }

    // ── Logging ───────────────────────────────────────────────────────────────

    fn dbg(&self, stage: &str, msg: &str, t_start: Option<std::time::Instant>) {
        if !self.verbose {
            return;
        }
        let elapsed = match t_start {
            Some(t) => format!("+{:<5.3}s ", t.elapsed().as_secs_f64()).bright_black().to_string(),
            None => String::new(),
        };
        let prefix = match stage {
            "SESSION" => "[SESSION ]".bright_blue().to_string(),
            "CAPTCHA" => "[CAPTCHA ]".bright_yellow().to_string(),
            "API"     => "[API     ]".bright_magenta().to_string(),
            "FLOW"    => "[FLOW    ]".bright_green().to_string(),
            "WARN"    => "[WARN    ]".bright_red().to_string(),
            "PERF"    => "[PERF    ]".bright_green().to_string(),
            "VERBOSE" => "[VERBOSE ]".bright_cyan().to_string(),
            _ => stage.to_string(),
        };
        eprintln!("  {} {} {}{}", "▸".bright_black(), prefix, elapsed, msg);
    }

    // ── Client construction ───────────────────────────────────────────────────

    /// Build a new `reqwest::Client` that shares the given `jar`.
    /// Called at most once per session; subsequent cookie arrivals are pushed
    /// directly into the jar so the pool stays warm.
    fn build_client(
        ua: &str,
        jar: Arc<reqwest::cookie::Jar>,
        resolved_ip: Option<SocketAddr>,
    ) -> reqwest::Client {
        let mut headers = HeaderMap::new();
        // Never blindly trust disk cache. Fallback gracefully instead of panicking on invalid strings.
        let fallback_ua = HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/137.0.0.0");
        let ua_header = HeaderValue::from_str(ua).unwrap_or(fallback_ua);
        headers.insert(USER_AGENT, ua_header);
        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
        headers.insert(
            HeaderName::from_static("x-requested-with"),
            HeaderValue::from_static("XMLHttpRequest"),
        );
        headers.insert(
            HeaderName::from_static("sec-fetch-dest"),
            HeaderValue::from_static("empty"),
        );
        headers.insert(
            HeaderName::from_static("sec-fetch-mode"),
            HeaderValue::from_static("cors"),
        );
        headers.insert(
            HeaderName::from_static("sec-fetch-site"),
            HeaderValue::from_static("same-origin"),
        );
        headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        headers.insert(REFERER, HeaderValue::from_static(PNR_PAGE));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://www.indianrail.gov.in"),
        );

        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .default_headers(headers)
            .brotli(true)
            .gzip(true)
            .cookie_provider(jar)
            .pool_idle_timeout(Duration::from_secs(300))
            .tcp_keepalive(Some(Duration::from_secs(60)));

        if let Some(addr) = resolved_ip {
            builder = builder.resolve("www.indianrail.gov.in", addr);
        }

        builder.build().unwrap()
    }

    // ── Cookie / session helpers ──────────────────────────────────────────────

    fn extract_cookies(headers: &HeaderMap, cookies_map: &mut HashMap<String, String>) -> bool {
        let mut modified = false;
        for cookie in headers.get_all(SET_COOKIE) {
            if let Ok(c_str) = cookie.to_str()
                && let Some(part) = c_str.split(';').next()
                    && let Some((k, v)) = part.split_once('=') {
                        cookies_map.insert(k.trim().to_string(), v.trim().to_string());
                        modified = true;
                    }
        }
        modified
    }

    fn save_session(cookies: &HashMap<String, String>, ts: f64) {
        let path = session_file();
        let cache = SessionCache {
            cookies: cookies
                .iter()
                .map(|(k, v)| CookieEntry {
                    name: k.clone(),
                    value: v.clone(),
                })
                .collect(),
            ts,
        };
        ensure_cache_dir();
        if let Ok(data) = serde_json::to_string(&cache)
            && let Err(e) = std::fs::write(&path, data) {
                eprintln!(
                    "  {} {} Failed to save session cookies to disk: {}",
                    "▸".bright_black(), "[WARN    ]".bright_red(), e
                );
            }
    }

    fn load_session() -> Option<(HashMap<String, String>, f64)> {
        let path = session_file();
        if let Ok(data) = std::fs::read_to_string(&path)
            && let Ok(cache) = serde_json::from_str::<SessionCache>(&data) {
                let mut map = HashMap::new();
                for entry in cache.cookies {
                    map.insert(entry.name, entry.value);
                }
                return Some((map, cache.ts));
            }
        None
    }

    // ── Session lifecycle ─────────────────────────────────────────────────────

    /// Returns a ready-to-use client, initialising or reusing the session as
    /// needed.  The mutex is **never** held across a network `.send().await`.
    async fn get_client(&self) -> reqwest::Client {
        let now = now_sec();

        // ── Fast path: valid session in RAM ───────────────────────────────────
        {
            let st = self.state.lock().await;
            if let Some(ref client) = st.client
                && now - st.session_ts <= SESSION_TTL {
                    self.dbg(
                        "SESSION",
                        &format!(
                            "Reusing existing RAM session age={:.0}s / TTL={}s",
                            now - st.session_ts,
                            SESSION_TTL
                        ),
                        None,
                    );
                    return client.clone();
                }
        }

        // ── Try loading from disk ─────────────────────────────────────────────
        if let Some((disk_cookies, disk_ts)) = Self::load_session()
            && now - disk_ts <= SESSION_TTL {
                let mut st = self.state.lock().await;
                // Re-check under the lock in case another task beat us here.
                if st.client.is_none() {
                    self.dbg(
                        "SESSION",
                        &format!(
                            "Loaded existing session from disk age={:.0}s / TTL={}s",
                            now - disk_ts,
                            SESSION_TTL
                        ),
                        None,
                    );
                    add_cookies_to_jar(&st.jar, &disk_cookies);
                    st.cookies = disk_cookies;
                    st.session_ts = disk_ts;
                    let client = Self::build_client(&st.ua, Arc::clone(&st.jar), st.resolved_ip);
                    st.client = Some(client.clone());
                    return client;
                }
                return st.client.as_ref().unwrap().clone();
            }

        // ── Full re-init: resolve DNS & fetch fresh JSESSIONID ────────────────
        let t = std::time::Instant::now();
        
        let mut resolved_ip = {
            let st = self.state.lock().await;
            if let Some(ip) = st.resolved_ip {
                self.dbg("SESSION", &format!("Reusing cached DNS: {}", ip), None);
                Some(ip)
            } else {
                None
            }
        };
        if resolved_ip.is_none() {
            resolved_ip = self.refresh_dns().await;
        }

        self.dbg(
            "SESSION",
            "Initialising new session (fetching PNR page for JSESSIONID)...",
            None,
        );

        // Grab what we need, clear stale state, then drop the lock before I/O.
        let (ua, jar) = {
            let mut st = self.state.lock().await;
            st.client = None;
            st.session_ts = 0.0;
            st.cookies.clear();
            st.resolved_ip = resolved_ip;
            (st.ua.clone(), Arc::clone(&st.jar))
        };

        // Build the init client (shares the same jar) and fetch outside the lock.
        let init_client = Self::build_client(&ua, Arc::clone(&jar), resolved_ip);
        let mut new_cookies = HashMap::new();
        if let Ok(resp) = init_client.get(PNR_PAGE).send().await {
            Self::extract_cookies(resp.headers(), &mut new_cookies);
        }

        // Re-acquire the lock to commit results.
        {
            let mut st = self.state.lock().await;
            let ts = now_sec();
            add_cookies_to_jar(&jar, &new_cookies);
            st.cookies = new_cookies;
            st.session_ts = ts;
            if !st.cookies.is_empty() {
                Self::save_session(&st.cookies, ts);
            }
            // Reuse init_client — it already has the fresh cookies via the shared jar.
            st.client = Some(init_client.clone());
        }

        self.dbg(
            "SESSION",
            &format!("Session ready ({}ms)", t.elapsed().as_millis()),
            None,
        );
        init_client
    }

    async fn invalidate_session(&self) {
        let mut st = self.state.lock().await;
        self.dbg("SESSION", "Session invalidated — purging RAM & Disk", None);
        st.client = None;
        st.session_ts = 0.0;
        st.cookies.clear();
        let _ = std::fs::remove_file(session_file());
    }

    // ── Captcha helpers ───────────────────────────────────────────────────────

    async fn check_captcha_required(&self, client: &reqwest::Client) -> bool {
        let t = std::time::Instant::now();
        match client.get(CAPTCHA_CONFIG_URL).send().await {
            Ok(resp) => {
                if let Ok(text) = resp.text().await {
                    let text = text.trim();
                    let enabled = if let Ok(n) = text.parse::<u32>() {
                        n == 1
                    } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                        val.get("captchaEnabled")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1)
                            == 1
                    } else {
                        true
                    };
                    self.dbg(
                        "CAPTCHA",
                        &format!(
                            "CaptchaConfig → {} ({}ms)",
                            if enabled { "REQUIRED" } else { "NOT required" },
                            t.elapsed().as_millis()
                        ),
                        None,
                    );
                    enabled
                } else {
                    true
                }
            }
            Err(e) => {
                self.dbg(
                    "WARN",
                    &format!(
                        "CaptchaConfig fetch failed ({}) — defaulting to REQUIRED",
                        e
                    ),
                    None,
                );
                true
            }
        }
    }

    /// Fetch the captcha image. Returns raw bytes — no extra copy beyond what
    /// `reqwest` already holds internally.
    async fn fetch_captcha(&self, client: &reqwest::Client) -> Option<Bytes> {
        let t = std::time::Instant::now();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let url = format!("{}?{}", CAPTCHA_URL, ts);
        if let Ok(resp) = client
            .get(&url)
            .header("Accept", "image/png,image/*;q=0.9")
            .send()
            .await
            && let Ok(bytes) = resp.bytes().await {
                self.dbg(
                    "CAPTCHA",
                    &format!(
                        "Fetched captcha image size={} ({}ms)",
                        bytes.len(),
                        t.elapsed().as_millis()
                    ),
                    None,
                );
                return Some(bytes);
            }
        None
    }

    // ── Response analysis ─────────────────────────────────────────────────────

    fn is_waitlisted(raw: &crate::models::RawApiResponse<'_>) -> bool {
        // (?i) flag: match case-insensitively without allocating a to_uppercase() String.
        static RE_WL: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?i)WL|RLWL|GNWL|PQWL|TQWL").unwrap());
        if let Some(passengers) = &raw.passenger_list {
            for p in passengers {
                let status = p.current_status.unwrap_or("");
                if RE_WL.is_match(status) {
                    return true;
                }
            }
        }
        false
    }

    fn map_response(raw: &crate::models::RawApiResponse<'_>) -> MappedResponse {
        let passengers = raw
            .passenger_list
            .as_ref()
            .map(|pl| {
                pl.iter()
                    .map(|p| {
                        let book = match p.booking_status_details {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => {
                                let combined = format!(
                                    "{}/{}",
                                    p.booking_status.unwrap_or(""),
                                    p.booking_berth_no.as_ref().map(|s| s.as_cow()).unwrap_or_default()
                                );
                                combined.trim_end_matches('/').to_string()
                            }
                        };
                        let mut curr = match p.current_status_details {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => {
                                let combined = format!(
                                    "{}/{}",
                                    p.current_status.unwrap_or(""),
                                    p.current_berth_no.as_ref().map(|s| s.as_cow()).unwrap_or_default()
                                );
                                combined.trim_end_matches('/').to_string()
                            }
                        };
                        let coach = p.current_coach_id.as_ref().map(|s| s.as_cow()).unwrap_or_default();
                        if !coach.is_empty() {
                            curr = format!("{}/{}", coach, curr);
                        }
                        let serial = p.passenger_serial_number.as_ref().map(|s| s.as_cow().into_owned()).unwrap_or_default();
                        let quota = p.passenger_quota.unwrap_or("").to_string();

                        MappedPassenger {
                            serial,
                            booking_status: book,
                            current_status: curr,
                            coach_position: coach.into_owned(),
                            quota,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut chart_status = raw.chart_status.unwrap_or("").to_string();
        if chart_status.is_empty() {
            chart_status = "-".to_string();
        }
        if let Some(msgs) = &raw.information_message {
            let valid_msgs: Vec<&str> = msgs
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect();
            if !valid_msgs.is_empty() {
                chart_status = format!("{} | {}", chart_status, valid_msgs.join(" | "));
            }
        }

        let generated_at = raw.generated_time_stamp.as_ref().map(crate::ui::parse_ts_from_raw).unwrap_or_else(|| "-".to_string());

        MappedResponse {
            pnr: raw.pnr_number.as_ref().map(|s| s.as_cow().into_owned()).unwrap_or_default(),
            journey: JourneyInfo {
                train_number: raw.train_number.as_ref().map(|s| s.as_cow().into_owned()).unwrap_or_default(),
                train_name: raw.train_name.unwrap_or("").to_string(),
                boarding_date: raw.date_of_journey.unwrap_or("").to_string(),
                from: raw.source_station.unwrap_or("").to_string(),
                to: raw.destination_station.unwrap_or("").to_string(),
                reserved_upto: raw.reservation_upto.unwrap_or("").to_string(),
                boarding_point: raw.boarding_point.unwrap_or("").to_string(),
                class: raw.journey_class.unwrap_or("").to_string(),
            },
            passengers,
            fare: FareInfo {
                total_fare: crate::ui::extract_fare_from_raw(raw),
                charting_status: chart_status,
            },
            generated_at,
        }
    }

    // ── Core fetch loop ───────────────────────────────────────────────────────

    async fn run_fetch_loop<F>(
        &self,
        mut needs_captcha: bool,
        input_page: &str,
        pnr_number: Option<&str>,
        progress: &F,
        prefetched_img: Option<Bytes>,
    ) -> (Option<Result<(Bytes, serde_json::Value), String>>, bool)
    where
        F: Fn(&str),
    {
        let label = if input_page == "PNR" {
            "PNR"
        } else {
            "Prediction"
        };
        let mut current_img = prefetched_img;
        // Hoist client out of loop — only re-acquire on session invalidation.
        let mut client = self.get_client().await;

        for attempt in 1..=MAX_CAPTCHA_RETRIES {


            let mut captcha_answer: Option<String> = None;
            if needs_captcha {
                progress(&format!(
                    "[{}] Solving captcha (attempt {}/{})...",
                    label, attempt, MAX_CAPTCHA_RETRIES
                ));
                self.dbg(
                    "FLOW",
                    &format!("[{}] Attempt {} — captcha required", label, attempt),
                    None,
                );

                let bytes: Bytes = if current_img.is_some() {
                    current_img.take().unwrap()
                } else {
                    match self.fetch_captcha(&client).await {
                        Some(b) => b,
                        None => {
                            self.invalidate_session().await;
                            client = self.get_client().await;
                            needs_captcha =
                                self.check_captcha_required(&client).await;
                            continue;
                        }
                    }
                };

                match self.ocr.solve(bytes).await {
                    Some(ans) => captcha_answer = Some(ans.to_string()),
                    None => continue,
                }
            } else {
                progress(&format!(
                    "[{}] Captcha not required, submitting directly...",
                    label
                ));
            }

            let captcha_str = captcha_answer.unwrap_or_default();
            let mut params = vec![("inputCaptcha", captcha_str.as_str())];
            if let Some(pnr) = pnr_number {
                params.push(("inputPnrNo", pnr));
            }
            params.push(("inputPage", input_page));
            params.push(("facnam", input_page));
            params.push(("language", "en"));

            // Race Condition Fix: We no longer pre-fetch the NEXT captcha 
            // while submitting the current one. This ensures the current
            // submission isn't invalidated by a new captcha request.

            if self.verbose {
                let mut url = format!("{}?", API_URL);
                for (k, v) in &params {
                    url.push_str(&format!("{}={}&", k, v));
                }
                self.dbg("VERBOSE", &format!("Submitting: {}", url.trim_end_matches('&')), None);
            }

            let t_req = std::time::Instant::now();
            let resp = match client.get(API_URL).query(&params).send().await {
                Ok(r) => r,
                Err(e) => {
                    self.dbg(
                        "WARN",
                        &format!("[{}] API call failed: {} — possible IP rotation, refreshing DNS", label, e),
                        None,
                    );
                    {
                        let mut st = self.state.lock().await;
                        st.resolved_ip = None; // Force refresh on next get_client
                    }
                    self.invalidate_session().await;
                    client = self.get_client().await;
                    needs_captcha = self.check_captcha_required(&client).await;
                    continue;
                }
            };

            // Inject new cookies directly into the shared jar — client stays alive.
            {
                let mut new_cookies = HashMap::new();
                if Self::extract_cookies(resp.headers(), &mut new_cookies) {
                    if self.verbose {
                        for (k, v) in &new_cookies {
                            self.dbg("VERBOSE", &format!("Set-Cookie: {}={}", k, v), None);
                        }
                    }
                    let mut st = self.state.lock().await;
                    add_cookies_to_jar(&st.jar, &new_cookies);
                    st.cookies.extend(new_cookies);
                    Self::save_session(&st.cookies, now_sec());
                }
            }

            let body_bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    self.dbg("WARN", &format!("[{}] Bytes fetch failed: {}", label, e), None);
                    continue;
                }
            };

            let data: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                Ok(d) => d,
                Err(e) => {
                    self.dbg(
                        "WARN",
                        &format!("[{}] JSON parse failed: {} — invalidating session", label, e),
                        None,
                    );
                    if self.verbose {
                        let snippet = String::from_utf8_lossy(&body_bytes);
                        eprintln!("  {} {} Raw Response Snippet: {}", "▸".bright_black(), "[DIAGNOSTIC]".bright_red(), snippet.chars().take(1000).collect::<String>());
                    }
                    self.invalidate_session().await;
                    client = self.get_client().await;
                    needs_captcha = self.check_captcha_required(&client).await;
                    continue;
                }
            };

            let flag = data.get("flag").and_then(|v| v.as_str());

            self.dbg(
                "API",
                &format!(
                    "Response (flag={:?}) ({}ms)",
                    flag,
                    t_req.elapsed().as_millis()
                ),
                None,
            );

            if self.verbose {
                let snippet = String::from_utf8_lossy(&body_bytes);
                self.dbg("VERBOSE", &format!("Raw Response: {}", snippet), None);
            }

            let err_msg = data.get("errorMessage").and_then(|v| v.as_str()).unwrap_or("");

            if err_msg == "Session out or Invalid Request" {
                self.dbg("WARN", &format!("[{}] Session expired on server", label), None);
                self.invalidate_session().await;
                client = self.get_client().await;
                needs_captcha = self.check_captcha_required(&client).await;
                continue;
            }

            if err_msg.contains("Captcha") || flag == Some("NO") {
                self.dbg("WARN", &format!("[{}] Server rejected captcha: \"{}\" — retrying...", label, err_msg), None);
                if !needs_captcha {
                    progress(&format!("[{}] Server requested captcha unexpectedly", label));
                    needs_captcha = true;
                }
                // Fetch the new captcha image ONLY after the current attempt has failed.
                current_img = None; // Reset so next loop fetches fresh
                continue;
            }

            if !err_msg.is_empty() {
                return (Some(Err(err_msg.to_string())), needs_captcha);
            }

            self.dbg("FLOW", &format!("[{}] Success on attempt {}", label, attempt), None);
            
            return (Some(Ok((body_bytes, data))), needs_captcha);
        }

        (None, needs_captcha)
    }

    // ── Public API ────────────────────────────────────────────────────────────

    pub async fn get_pnr_status<F>(&self, pnr: &str, progress: F) -> PnrResult
    where
        F: Fn(&str),
    {
        let t_start = std::time::Instant::now();
        let client = self.get_client().await;

        // Speculative parallel fetch using "No Wasted Bytes" rule:
        // Use tokio::select! to start both the config check and image fetch.
        // If config returns 'not required' first, we drop the image fetch immediately.
        let mut speculative_img = None;
        let mut fetch_fut = std::pin::pin!(self.fetch_captcha(&client));
        
        self.dbg("SESSION", "Speculative captcha pre-load started...", None);
        let needs_captcha = tokio::select! {
            required = self.check_captcha_required(&client) => {
                self.dbg("SESSION", "Captcha check finished faster than pre-load.", None);
                required
            },
            img = &mut fetch_fut => {
                self.dbg("SESSION", "Pre-load finished faster than captcha check.", None);
                speculative_img = img;
                self.check_captcha_required(&client).await
            }
        };
        
        let prefetched_img = if needs_captcha {
            if speculative_img.is_some() {
                self.dbg("SESSION", "Using pre-loaded captcha image.", None);
                speculative_img
            } else {
                self.dbg("SESSION", "Waiting for pre-load to finish...", None);
                fetch_fut.await
            }
        } else {
            self.dbg("SESSION", "Dropping pre-loaded image — captcha not required.", None);
            None
        };

        let (result, needs_captcha) = self
            .run_fetch_loop(needs_captcha, "PNR", Some(pnr), &progress, prefetched_img)
            .await;

        let (raw_bytes, full_raw) = match result {
            Some(Ok(pair)) => pair,
            Some(Err(err)) => {
                return PnrResult {
                    success: false,
                    error: Some(err),
                    raw: None,
                    mapped: None,
                    prediction: None,
                    elapsed: t_start.elapsed().as_secs_f64(),
                };
            }
            None => {
                return PnrResult {
                    success: false,
                    error: Some(format!(
                        "Failed to solve captcha after {} attempts.",
                        MAX_CAPTCHA_RETRIES
                    )),
                    raw: None,
                    mapped: None,
                    prediction: None,
                    elapsed: t_start.elapsed().as_secs_f64(),
                };
            }
        };

        // ZERO-ALLOCATION PARSING: Borrow strings directly from raw_bytes.
        let raw_api: crate::models::RawApiResponse = match serde_json::from_slice(&raw_bytes) {
            Ok(d) => d,
            Err(e) => {
                return PnrResult {
                    success: false,
                    error: Some(format!("Failed to parse API response: {}", e)),
                    raw: None,
                    mapped: None,
                    prediction: None,
                    elapsed: t_start.elapsed().as_secs_f64(),
                };
            }
        };

        let mapped = Self::map_response(&raw_api);

        let mut prediction = None;
        if Self::is_waitlisted(&raw_api) {
            progress("Waitlist detected! Fetching confirmation probability...");
            let (p_res, _) = self
                .run_fetch_loop(needs_captcha, "Prediction", None, &progress, None)
                .await;
            if let Some(Ok((_p_bytes, p_value))) = p_res {
                prediction = Some(p_value);
            }
        }

        PnrResult {
            success: true,
            error: None,
            raw: Some(full_raw),
            mapped: Some(mapped),
            prediction,
            elapsed: t_start.elapsed().as_secs_f64(),
        }
    }
}

