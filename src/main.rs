mod api;
mod captcha;
mod models;
mod ui;

use clap::Parser;
use colored::*;
use std::io::Write;

#[derive(Parser, Debug)]
#[command(author, version, about = "Indian Railways PNR Status Checker", long_about = None)]
struct Args {
    /// 10-digit PNR number
    #[arg(short, long)]
    pnr: Option<String>,

    /// Export JSON to file (e.g. data.json)
    #[arg(short, long)]
    export: Option<String>,

    /// Print raw JSON to console
    #[arg(long)]
    show_json: bool,

    /// Step-by-step debug logging with per-stage timings
    #[arg(short, long)]
    verbose: bool,

    /// Local disk session TTL before force re-init (seconds)
    #[arg(long, default_value_t = 900)]
    ttl: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    let _ = colored::control::set_virtual_terminal(true);

    let args = Args::parse();

    let mut pnr = args.pnr.clone();
    if pnr.is_none() {
        print!("{}", "Enter 10-digit PNR: ".bold());
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        pnr = Some(input.trim().to_string());
    }

    let pnr = pnr.unwrap();
    if pnr.len() != 10 || !pnr.chars().all(char::is_numeric) {
        eprintln!("{}", "Invalid PNR. Must be exactly 10 digits.".red());
        std::process::exit(1);
    }

    println!(
        "\n{}",
        format!("Fetching details for PNR {}...", pnr).dimmed()
    );

    let api_client = api::ApiClient::new(args.verbose).await;

    let progress_cb = |msg: &str| {
        if args.verbose {
            println!("  {} {}", "→".cyan().dimmed(), msg.cyan().dimmed());
        }
    };

    let result = api_client.get_pnr_status(&pnr, progress_cb).await;

    if !result.success {
        println!(
            "\n  {} {}\n",
            " ERROR ".white().on_bright_red().bold(),
            result.error.unwrap_or_default().red()
        );
        std::process::exit(1);
    }

    let raw = result.raw.unwrap();
    let pred = result.prediction.as_ref();
    let mapped = result.mapped.unwrap();

    if args.show_json {
        let mut payload = serde_json::Map::new();
        payload.insert("pnr".to_string(), raw.clone());
        if let Some(p) = pred {
            payload.insert("prediction".to_string(), p.clone().clone());
        }
        let merged = serde_json::Value::Object(payload);
        println!(
            "\n{}\n  RAW JSON\n{}",
            "─".repeat(62).dimmed(),
            "─".repeat(62).dimmed()
        );
        println!("{}", serde_json::to_string_pretty(&merged).unwrap());
    }

    ui::display(&raw, result.elapsed, pred);

    if let Some(export_path) = args.export {
        let mut out_data = serde_json::Map::new();
        out_data.insert(
            "pnr_status".to_string(),
            serde_json::to_value(&mapped).unwrap(),
        );
        if let Some(p) = pred {
            out_data.insert("prediction".to_string(), p.clone().clone());
        }

        match std::fs::write(
            &export_path,
            serde_json::to_string_pretty(&serde_json::Value::Object(out_data)).unwrap(),
        ) {
            Ok(_) => {
                println!(
                    "  {} Mapped data exported to {}\n",
                    "✓".green(),
                    export_path.bold()
                );
            }
            Err(e) => {
                eprintln!("  {} Export failed: {}\n", "✗".red(), e);
            }
        }
    }

    // Drop the thread-local Tesseract engine before process exit so that
    // Tesseract's ObjectCache singleton is still alive when the reference
    // counts are released — suppresses the "WARNING! LEAK!" dawg messages.
    captcha::cleanup();

    Ok(())
}
