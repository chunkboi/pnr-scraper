use colored::*;

const MONTHS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const W: usize = 62;

fn sep() {
    println!("  {}", "━".repeat(W).blue());
}

fn thin() {
    println!("  {}  {}", "│".blue(), "─".repeat(W - 4).bright_black());
}

fn title(t: &str) {
    let padding = (W - t.chars().count()) / 2;
    let pad_str = " ".repeat(padding);
    let right_pad = " ".repeat(W - t.chars().count() - padding);
    let s = format!("{}{}{}", pad_str, t, right_pad);
    println!("  {}", s.on_blue().white().bold());
}

fn kv(k: &str, v: &str, vc: colored::Color) {
    println!(
        "  {}  {: <22} {}",
        "│".blue(),
        k.bright_black(),
        v.color(vc).bold()
    );
}

/// Extract a display-ready string from a JSON value, handling both strings and numbers.
fn jstr(v: &serde_json::Value, key: &str) -> String {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        _ => "-".to_string(),
    }
}

fn colorize_status(status: &str) -> ColoredString {
    let s = status.to_uppercase();
    if s.contains("CNF") || s.contains("CONFIRMED") {
        format!("  ✓ {}  ", status).black().on_bright_green().bold()
    } else if s.contains("RAC") {
        format!("  ⚠ {}  ", status).black().on_bright_yellow().bold()
    } else if s.contains("WL") || s.contains("WAIT") || s.contains("CAN")
        || s.contains("RLWL") || s.contains("GNWL") || s.contains("PQWL") || s.contains("TQWL") {
        format!("  ✗ {}  ", status).white().on_bright_red().bold()
    } else {
        status.white().bold()
    }
}

fn prob_bar(p: f64, width: usize) -> String {
    let filled = (p * width as f64).round() as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width.saturating_sub(filled)));
    let pct = format!("{:.1}%", p * 100.0);
    
    let col = if p >= 0.7 {
        colored::Color::Green
    } else if p >= 0.4 {
        colored::Color::Yellow
    } else {
        colored::Color::Red
    };
    
    format!("{} {}", bar.color(col), pct.color(col).bold())
}

fn parse_ts(v: &serde_json::Value) -> String {
    if let Some(ts) = v.get("generatedTimeStamp") {
        let day = ts.get("day").and_then(|v| v.as_u64()).unwrap_or(0);
        let month = ts.get("month").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let year = ts.get("year").and_then(|v| v.as_u64()).unwrap_or(0);
        let hour = ts.get("hour").and_then(|v| v.as_u64()).unwrap_or(0);
        let minute = ts.get("minute").and_then(|v| v.as_u64()).unwrap_or(0);
        let second = ts.get("second").and_then(|v| v.as_u64()).unwrap_or(0);
        let month_idx = if (1..=12).contains(&month) { month - 1 } else { 0 };
        format!(
            "{:02}-{}-{} [{:02}:{:02}:{:02} IST]",
            day, MONTHS[month_idx], year, hour, minute, second
        )
    } else {
        "-".to_string()
    }
}

fn parse_jdate(val: Option<&serde_json::Value>) -> String {
    if let Some(v) = val
        && let Some(num) = v.as_i64()
            && let chrono::LocalResult::Single(dt) = chrono::TimeZone::timestamp_opt(&chrono::Utc, num / 1000, 0) {
                return dt.format("%d-%b-%Y").to_string();
            }
    "-".to_string()
}

/// Extract fare from the raw JSON, checking multiple possible fields.
pub fn extract_fare(v: &serde_json::Value) -> String {
    for key in ["totalFare", "ticketFare", "bookingFare", "fare"] {
        if let Some(val) = v.get(key) {
            match val {
                serde_json::Value::String(s) => {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && trimmed != "0" && trimmed != "0.0" && trimmed.to_lowercase() != "null" {
                        return trimmed.to_string();
                    }
                }
                serde_json::Value::Number(n) => {
                    if let Some(f) = n.as_f64()
                        && f > 0.0 {
                            return n.to_string();
                        }
                }
                _ => {}
            }
        }
    }
    "".to_string()
}

pub fn display(raw_resp: &serde_json::Value, elapsed: f64, pred: Option<&serde_json::Value>) {
    println!();
    sep();
    title("🚆  PNR STATUS RESULT");
    sep();
    
    kv("PNR Number", &jstr(raw_resp, "pnrNumber"), colored::Color::Cyan);
    kv("As of", &parse_ts(raw_resp), colored::Color::White);
    sep();

    // ── Journey Details ─────────────────────────────────────────────
    println!("  {}  {}", "│".blue(), "JOURNEY DETAILS".magenta().bold());
    thin();
    let train = format!("{} — {}", jstr(raw_resp, "trainNumber"), jstr(raw_resp, "trainName"));
    kv("Train", &train, colored::Color::Yellow);
    kv("Date", &jstr(raw_resp, "dateOfJourney"), colored::Color::White);
    let route = format!("{} → {}", jstr(raw_resp, "sourceStation"), jstr(raw_resp, "destinationStation"));
    kv("From → To", &route, colored::Color::Green);
    kv("Reserved Upto", &jstr(raw_resp, "reservationUpto"), colored::Color::White);
    kv("Boarding Point", &jstr(raw_resp, "boardingPoint"), colored::Color::White);
    kv("Class", &jstr(raw_resp, "journeyClass"), colored::Color::Cyan);
    sep();

    // ── Passenger Status ────────────────────────────────────────────
    if let Some(passengers) = raw_resp.get("passengerList").and_then(|v| v.as_array())
        && !passengers.is_empty() {
            let n = passengers.len();
            let p_word = if n > 1 { "passengers" } else { "passenger" };
            println!(
                "  {}  {}",
                "│".blue(),
                format!("PASSENGER STATUS  ({} {})", n, p_word).magenta().bold()
            );
            thin();
            for (i, p) in passengers.iter().enumerate() {
                if i > 0 { println!("  {}", "│".blue()); }
                
                let book = match p.get("bookingStatusDetails").and_then(|v| v.as_str()) {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        let status = jstr(p, "bookingStatus");
                        let berth = jstr(p, "bookingBerthNo");
                        let combined = format!("{}/{}", status, berth);
                        combined.trim_end_matches('/').to_string()
                    }
                };
                
                let mut curr = match p.get("currentStatusDetails").and_then(|v| v.as_str()) {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        let status = jstr(p, "currentStatus");
                        let berth = jstr(p, "currentBerthNo");
                        let combined = format!("{}/{}", status, berth);
                        combined.trim_end_matches('/').to_string()
                    }
                };
                
                let coach = jstr(p, "currentCoachId");
                if coach != "-" && !coach.is_empty() {
                    curr = format!("{}/{}", coach, curr);
                }
                
                let sno = match p.get("passengerSerialNumber") {
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    _ => "?".to_string(),
                };
                let quota = match p.get("passengerQuota").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => "-".to_string(),
                };

                println!("  {}   {}  {}", "│".blue(), format!("Passenger {}", sno).cyan().bold(), format!("({})", quota).bright_black());
                println!("  {}     {}  {}", "│".blue(), "Booking :".bright_black(), book.white());
                println!("  {}     {}  {}", "│".blue(), "Current :".bright_black(), colorize_status(&curr));
            }
            sep();
        }

    // ── Fare & Charting ─────────────────────────────────────────────
    let fare = extract_fare(raw_resp);
    let chart = match raw_resp.get("chartStatus").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => "-".to_string(),
    };
    let cc = if chart.contains("Prepared") && !chart.contains("Not") {
        colored::Color::Green
    } else {
        colored::Color::Yellow
    };

    println!("  {}  {}", "│".blue(), "FARE & CHARTING".magenta().bold());
    thin();
    if !fare.is_empty() {
        kv("Total Fare", &format!("₹ {}", fare), colored::Color::Green);
    } else {
        kv("Total Fare", "Not available", colored::Color::BrightBlack);
    }
    kv("Chart Status", &chart, cc);
    
    if let Some(msgs) = raw_resp.get("informationMessage").and_then(|v| v.as_array()) {
        let valid_msgs: Vec<&str> = msgs.iter().filter_map(|m| m.as_str()).filter(|s| !s.is_empty()).collect();
        if !valid_msgs.is_empty() {
            kv("Info", &valid_msgs.join(" | "), colored::Color::Yellow);
        }
    }
    sep();

    // ── WL Confirmation Prediction ──────────────────────────────────
    if let Some(p_data) = pred {
        println!("  {}  {}", "│".blue(), "🎲  WL CONFIRMATION PREDICTION".magenta().bold());
        thin();
        if let Some(prob) = p_data.get("probability").and_then(|v| v.as_f64()) {
            println!(
                "  {}  {: <22} {}",
                "│".blue(),
                "CNF Probability".bright_black(),
                prob_bar(prob, 28)
            );
        }
        if let Some(wl_list) = p_data.get("maxWlRacCnfList").and_then(|v| v.as_array())
            && !wl_list.is_empty() {
                println!("  {}", "│".blue());
                println!(
                    "  {}  {: <16}{}",
                    "│".blue(),
                    "Date".bright_black(),
                    "Last Year Status".bright_black()
                );
                println!("  {}  {}", "│".blue(), "─".repeat(40).bright_black());
                for entry in wl_list.iter().take(10) {
                    let jdate = parse_jdate(entry.get("jdate"));
                    let status = format!(
                        "{}/{}",
                        entry.get("lastYearRunningStatus").and_then(|v| v.as_str()).unwrap_or("-"),
                        entry.get("lastYearRunningNumber").and_then(|v| v.as_str()).unwrap_or("-")
                    );
                    println!(
                        "  {}  {: <16}{}",
                        "│".blue(),
                        jdate.bright_black(),
                        colorize_status(&status)
                    );
                }
            }
        sep();
    }

    println!("  {}\n", format!("⏱  Fetched in {:.2} seconds", elapsed).bright_black());
}
