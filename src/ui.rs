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

pub fn parse_ts_from_raw(ts: &crate::models::RawTimeStamp) -> String {
    let month_idx = if (1..=12).contains(&(ts.month as usize)) { (ts.month as usize) - 1 } else { 0 };
    format!(
        "{:02}-{}-{} [{:02}:{:02}:{:02} IST]",
        ts.day, MONTHS[month_idx], ts.year, ts.hour, ts.minute, ts.second
    )
}

fn parse_jdate(val: Option<&serde_json::Value>) -> String {
    if let Some(v) = val
        && let Some(num) = v.as_i64()
            && let chrono::LocalResult::Single(dt) = chrono::TimeZone::timestamp_opt(&chrono::Utc, num / 1000, 0) {
                return dt.format("%d-%b-%Y").to_string();
            }
    "-".to_string()
}

pub fn extract_fare_from_raw(v: &crate::models::RawApiResponse<'_>) -> String {
    let candidates = [
        v.total_fare.as_ref(),
        v.ticket_fare.as_ref(),
        v.booking_fare.as_ref(),
        v.fare.as_ref(),
    ];
    for val in candidates.into_iter().flatten() {
        let s = val.to_string();
        let trimmed = s.trim();
        if !trimmed.is_empty() && trimmed != "0" && trimmed != "0.0" && trimmed.to_lowercase() != "null" {
            return trimmed.to_string();
        }
    }
    "".to_string()
}

pub fn display(res: &crate::models::MappedResponse, elapsed: f64, pred: Option<&serde_json::Value>) {
    println!();
    sep();
    title("🚆  PNR STATUS RESULT");
    sep();
    
    kv("PNR Number", &res.pnr, colored::Color::Cyan);
    kv("As of", &res.generated_at, colored::Color::White);
    sep();

    // ── Journey Details ─────────────────────────────────────────────
    println!("  {}  {}", "│".blue(), "JOURNEY DETAILS".magenta().bold());
    thin();
    let train = format!("{} — {}", res.journey.train_number, res.journey.train_name);
    kv("Train", &train, colored::Color::Yellow);
    kv("Date", &res.journey.boarding_date, colored::Color::White);
    let route = format!("{} → {}", res.journey.from, res.journey.to);
    kv("From → To", &route, colored::Color::Green);
    kv("Reserved Upto", &res.journey.reserved_upto, colored::Color::White);
    kv("Boarding Point", &res.journey.boarding_point, colored::Color::White);
    kv("Class", &res.journey.class, colored::Color::Cyan);
    sep();

    // ── Passenger Status ────────────────────────────────────────────
    if !res.passengers.is_empty() {
        let n = res.passengers.len();
        let p_word = if n > 1 { "passengers" } else { "passenger" };
        println!(
            "  {}  {}",
            "│".blue(),
            format!("PASSENGER STATUS  ({} {})", n, p_word).magenta().bold()
        );
        thin();
            for (i, p) in res.passengers.iter().enumerate() {
                if i > 0 { println!("  {}", "│".blue()); }
                let quota_str = if p.quota.is_empty() { String::new() } else { format!("  ({})", p.quota) };
                println!("  {}   {}  {}", "│".blue(), format!("Passenger {}", p.serial).cyan().bold(), quota_str.bright_black());
                println!("  {}     {}  {}", "│".blue(), "Booking :".bright_black(), p.booking_status.white());
                println!("  {}     {}  {}", "│".blue(), "Current :".bright_black(), colorize_status(&p.current_status));
            }
        sep();
    }

    // ── Fare & Charting ─────────────────────────────────────────────
    let chart = &res.fare.charting_status;
    let cc = if chart.contains("Prepared") && !chart.contains("Not") {
        colored::Color::Green
    } else {
        colored::Color::Yellow
    };

    println!("  {}  {}", "│".blue(), "FARE & CHARTING".magenta().bold());
    thin();
    if !res.fare.total_fare.is_empty() {
        kv("Total Fare", &format!("₹ {}", res.fare.total_fare), colored::Color::Green);
    } else {
        kv("Total Fare", "Not available", colored::Color::BrightBlack);
    }
    kv("Chart Status", chart, cc);
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
