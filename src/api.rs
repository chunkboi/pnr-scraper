use crate::captcha;
use crate::models::{
    CookieEntry, FareInfo, JourneyInfo, MappedPassenger, MappedResponse, PnrResult, SessionCache,
    UaCache,
};
use bytes::Bytes;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, ORIGIN, REFERER, SET_COOKIE,
    USER_AGENT,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};
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
        .unwrap()
        .as_secs_f64()
}

fn session_file() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pnr_session.json")
}

fn ua_file() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pnr_ua_cache.json")
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
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(cache) = serde_json::from_str::<UaCache>(&data) {
            if now_sec() - cache.ts < UA_CACHE_TTL {
                return cache.ua;
            }
        }
    }
    let ua = FALLBACK.to_string();
    let cache = UaCache {
        ua: ua.clone(),
        ts: now_sec(),
    };
    let _ = std::fs::write(&path, serde_json::to_string(&cache).unwrap());
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
}

pub struct ApiClient {
    state: Arc<Mutex<AppState>>,
    verbose: bool,
}

// =============================================================================
// ApiClient implementation
// =============================================================================

impl ApiClient {
    pub async fn new(verbose: bool) -> Self {
        let ua = get_latest_user_agent().await;
        let jar = Arc::new(reqwest::cookie::Jar::default());
        ApiClient {
            state: Arc::new(Mutex::new(AppState {
                jar,
                client: None,
                session_ts: 0.0,
                ua,
                cookies: HashMap::new(),
            })),
            verbose,
        }
    }

    // ── Logging ───────────────────────────────────────────────────────────────

    fn dbg(&self, stage: &str, msg: &str, t_start: Option<std::time::Instant>) {
        if !self.verbose {
            return;
        }
        let elapsed = match t_start {
            Some(t) => format!("\x1b[90m+{:<5.3}s\x1b[0m ", t.elapsed().as_secs_f64()),
            None => String::new(),
        };
        let prefix = match stage {
            "SESSION" => "\x1b[94m[SESSION ]\x1b[0m",
            "CAPTCHA" => "\x1b[93m[CAPTCHA ]\x1b[0m",
            "API" => "\x1b[95m[API     ]\x1b[0m",
            "FLOW" => "\x1b[92m[FLOW    ]\x1b[0m",
            "WARN" => "\x1b[91m[WARN    ]\x1b[0m",
            "PERF" => "\x1b[92m[PERF    ]\x1b[0m",
            _ => stage,
        };
        eprintln!("  \x1b[90m▸\x1b[0m {} {}{}", prefix, elapsed, msg);
    }

    // ── Client construction ───────────────────────────────────────────────────

    /// Build a new `reqwest::Client` that shares the given `jar`.
    /// Called at most once per session; subsequent cookie arrivals are pushed
    /// directly into the jar so the pool stays warm.
    fn build_client(ua: &str, jar: Arc<reqwest::cookie::Jar>) -> reqwest::Client {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(ua).unwrap());
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/javascript, */*; q=0.01"),
        );
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
        headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        headers.insert(REFERER, HeaderValue::from_static(PNR_PAGE));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://www.indianrail.gov.in"),
        );
        headers.insert(
            "X-Requested-With",
            HeaderValue::from_static("XMLHttpRequest"),
        );

        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .default_headers(headers)
            .brotli(true)
            .gzip(true)
            .cookie_provider(jar)
            .build()
            .unwrap()
    }

    // ── Cookie / session helpers ──────────────────────────────────────────────

    fn extract_cookies(headers: &HeaderMap, cookies_map: &mut HashMap<String, String>) -> bool {
        let mut modified = false;
        for cookie in headers.get_all(SET_COOKIE) {
            if let Ok(c_str) = cookie.to_str() {
                if let Some(part) = c_str.split(';').next() {
                    if let Some((k, v)) = part.split_once('=') {
                        cookies_map.insert(k.trim().to_string(), v.trim().to_string());
                        modified = true;
                    }
                }
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
        if let Ok(data) = serde_json::to_string(&cache) {
            let _ = std::fs::write(&path, data);
        }
    }

    fn load_session() -> Option<(HashMap<String, String>, f64)> {
        let path = session_file();
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(cache) = serde_json::from_str::<SessionCache>(&data) {
                let mut map = HashMap::new();
                for entry in cache.cookies {
                    map.insert(entry.name, entry.value);
                }
                return Some((map, cache.ts));
            }
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
            if let Some(ref client) = st.client {
                if now - st.session_ts <= SESSION_TTL {
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
        }

        // ── Try loading from disk ─────────────────────────────────────────────
        if let Some((disk_cookies, disk_ts)) = Self::load_session() {
            if now - disk_ts <= SESSION_TTL {
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
                    let client = Self::build_client(&st.ua, Arc::clone(&st.jar));
                    st.client = Some(client.clone());
                    return client;
                }
                return st.client.as_ref().unwrap().clone();
            }
        }

        // ── Full re-init: fetch PNR page to obtain a fresh JSESSIONID ─────────
        let t = std::time::Instant::now();
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
            (st.ua.clone(), Arc::clone(&st.jar))
        };

        // Build the init client (shares the same jar) and fetch outside the lock.
        let init_client = Self::build_client(&ua, Arc::clone(&jar));
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
        {
            if let Ok(bytes) = resp.bytes().await {
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
        }
        None
    }

    // ── Response analysis ─────────────────────────────────────────────────────

    fn is_waitlisted_json(raw: &serde_json::Value) -> bool {
        // (?i) flag: match case-insensitively without allocating a to_uppercase() String.
        static RE_WL: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?i)WL|RLWL|GNWL|PQWL|TQWL").unwrap());
        if let Some(passengers) = raw.get("passengerList").and_then(|v| v.as_array()) {
            for p in passengers {
                let status = p
                    .get("currentStatusDetails")
                    .and_then(|v| v.as_str())
                    .or_else(|| p.get("currentStatus").and_then(|v| v.as_str()))
                    .unwrap_or("");
                if RE_WL.is_match(status) {
                    return true;
                }
            }
        }
        false
    }

    fn jval(v: &serde_json::Value, key: &str) -> String {
        match v.get(key) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    }

    fn map_response_json(raw: &serde_json::Value) -> MappedResponse {
        let passengers = raw
            .get("passengerList")
            .and_then(|v| v.as_array())
            .map(|pl| {
                pl.iter()
                    .map(|p| {
                        let book = match p.get("bookingStatusDetails").and_then(|v| v.as_str()) {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => {
                                let combined = format!(
                                    "{}/{}",
                                    Self::jval(p, "bookingStatus"),
                                    Self::jval(p, "bookingBerthNo")
                                );
                                combined.trim_end_matches('/').to_string()
                            }
                        };
                        let mut curr = match p.get("currentStatusDetails").and_then(|v| v.as_str())
                        {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => {
                                let combined = format!(
                                    "{}/{}",
                                    Self::jval(p, "currentStatus"),
                                    Self::jval(p, "currentBerthNo")
                                );
                                combined.trim_end_matches('/').to_string()
                            }
                        };
                        let coach = Self::jval(p, "currentCoachId");
                        if !coach.is_empty() {
                            curr = format!("{}/{}", coach, curr);
                        }
                        let serial = match p.get("passengerSerialNumber") {
                            Some(serde_json::Value::Number(n)) => n.to_string(),
                            Some(serde_json::Value::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        MappedPassenger {
                            serial,
                            booking_status: book,
                            current_status: curr,
                            coach_position: coach,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut chart_status = Self::jval(raw, "chartStatus");
        if chart_status.is_empty() {
            chart_status = "-".to_string();
        }
        if let Some(msgs) = raw.get("informationMessage").and_then(|v| v.as_array()) {
            let valid_msgs: Vec<&str> = msgs
                .iter()
                .filter_map(|m| m.as_str())
                .filter(|s| !s.is_empty())
                .collect();
            if !valid_msgs.is_empty() {
                chart_status = format!("{} | {}", chart_status, valid_msgs.join(" | "));
            }
        }

        MappedResponse {
            pnr: Self::jval(raw, "pnrNumber"),
            journey: JourneyInfo {
                train_number: Self::jval(raw, "trainNumber"),
                train_name: Self::jval(raw, "trainName"),
                boarding_date: Self::jval(raw, "dateOfJourney"),
                from: Self::jval(raw, "sourceStation"),
                to: Self::jval(raw, "destinationStation"),
                reserved_upto: Self::jval(raw, "reservationUpto"),
                boarding_point: Self::jval(raw, "boardingPoint"),
                class: Self::jval(raw, "journeyClass"),
            },
            passengers,
            fare: FareInfo {
                total_fare: crate::ui::extract_fare(raw),
                charting_status: chart_status,
            },
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
    ) -> (Option<serde_json::Value>, bool)
    where
        F: Fn(&str),
    {
        let label = if input_page == "PNR" {
            "PNR"
        } else {
            "Prediction"
        };
        let mut current_img = prefetched_img;

        for attempt in 1..=MAX_CAPTCHA_RETRIES {
            let t_attempt = std::time::Instant::now();
            let client = self.get_client().await;

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

                let bytes: Bytes = if attempt == 1 && current_img.is_some() {
                    current_img.take().unwrap()
                } else {
                    match self.fetch_captcha(&client).await {
                        Some(b) => b,
                        None => {
                            self.invalidate_session().await;
                            needs_captcha =
                                self.check_captcha_required(&self.get_client().await).await;
                            continue;
                        }
                    }
                };

                match captcha::solve_captcha(&bytes, self.verbose) {
                    Some(ans) => captcha_answer = Some(ans.to_string()),
                    None => continue,
                }
            } else {
                progress(&format!(
                    "[{}] Captcha not required, submitting directly...",
                    label
                ));
            }

            let mut params = vec![("inputPage", input_page), ("language", "en")];
            if let Some(pnr) = pnr_number {
                params.push(("inputPnrNo", pnr));
            }
            let ans_str = captcha_answer.unwrap_or_default();
            if needs_captcha {
                params.push(("inputCaptcha", &ans_str));
            }

            let t_req = std::time::Instant::now();
            let resp = match client.get(API_URL).query(&params).send().await {
                Ok(r) => r,
                Err(e) => {
                    self.dbg(
                        "WARN",
                        &format!("[{}] API call failed: {} — invalidating session", label, e),
                        None,
                    );
                    self.invalidate_session().await;
                    needs_captcha = self.check_captcha_required(&self.get_client().await).await;
                    continue;
                }
            };

            // Inject new cookies directly into the shared jar — client stays alive.
            {
                let mut new_cookies = HashMap::new();
                if Self::extract_cookies(resp.headers(), &mut new_cookies) {
                    let mut st = self.state.lock().await;
                    add_cookies_to_jar(&st.jar, &new_cookies);
                    st.cookies.extend(new_cookies);
                    Self::save_session(&st.cookies, now_sec());
                }
            }

            let data: serde_json::Value = match resp.json().await {
                Ok(d) => d,
                Err(_) => {
                    self.invalidate_session().await;
                    needs_captcha = self.check_captcha_required(&self.get_client().await).await;
                    continue;
                }
            };

            self.dbg(
                "API",
                &format!(
                    "Response ({:?}) ({}ms)",
                    data.get("flag"),
                    t_req.elapsed().as_millis()
                ),
                None,
            );

            let err_msg = data
                .get("errorMessage")
                .and_then(|s| s.as_str())
                .unwrap_or("");

            if err_msg == "Session out or Invalid Request" {
                self.dbg(
                    "WARN",
                    &format!(
                        "[{}] Session expired on server — purging & auto-reinitialising",
                        label
                    ),
                    None,
                );
                self.invalidate_session().await;
                needs_captcha = self.check_captcha_required(&self.get_client().await).await;
                continue;
            }

            if err_msg.contains("Captcha")
                || data.get("flag").and_then(|f| f.as_str()) == Some("NO")
            {
                if !needs_captcha {
                    progress(&format!(
                        "[{}] Server requested captcha unexpectedly, enabling...",
                        label
                    ));
                    needs_captcha = true;
                }
                continue;
            }

            if !err_msg.is_empty() {
                let mut err_map = serde_json::Map::new();
                err_map.insert(
                    "__error__".into(),
                    serde_json::Value::String(err_msg.to_string()),
                );
                return (Some(serde_json::Value::Object(err_map)), needs_captcha);
            }

            self.dbg(
                "FLOW",
                &format!(
                    "[{}] Success on attempt {} | total={}ms",
                    label,
                    attempt,
                    t_attempt.elapsed().as_millis()
                ),
                None,
            );
            return (Some(data), needs_captcha);
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

        let needs_captcha = self.check_captcha_required(&client).await;
        let prefetched_img = if needs_captcha {
            self.fetch_captcha(&client).await
        } else {
            None
        };

        let (result, needs_captcha) = self
            .run_fetch_loop(needs_captcha, "PNR", Some(pnr), &progress, prefetched_img)
            .await;

        if result.is_none() {
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

        let raw = result.unwrap();
        if let Some(err) = raw.get("__error__").and_then(|v| v.as_str()) {
            return PnrResult {
                success: false,
                error: Some(err.to_string()),
                raw: None,
                mapped: None,
                prediction: None,
                elapsed: t_start.elapsed().as_secs_f64(),
            };
        }

        let mapped = Self::map_response_json(&raw);

        let mut prediction = None;
        if Self::is_waitlisted_json(&raw) {
            progress("Waitlist detected! Fetching confirmation probability...");
            let (pred_result, _) = self
                .run_fetch_loop(needs_captcha, "PNRPrediction", None, &progress, None)
                .await;
            if let Some(p_data) = pred_result {
                if p_data.get("__error__").is_none() {
                    prediction = Some(p_data);
                }
            }
        }

        let elapsed = t_start.elapsed().as_secs_f64();
        progress(&format!("Status fetch complete in {:.2}s.", elapsed));

        PnrResult {
            success: true,
            error: None,
            raw: Some(raw),
            mapped: Some(mapped),
            prediction,
            elapsed,
        }
    }
}
