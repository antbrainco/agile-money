//! Enrols the owner's authenticator app. Shows a QR code to scan, asks for one
//! code to prove the scan worked, then writes `TOTP_SECRET` to `.env`. Restart
//! the bot afterwards. Run it yourself, in your own terminal: the secret is
//! shown on screen once and nowhere else.
//!
//! ```sh
//! cargo run -p warden-telegram-bot --example totp_setup
//! ```

use std::io::{BufRead, Write};

use anyhow::{Context, bail};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use totp_rs::{Builder, Secret};

const ENV: &str = ".env";

fn main() -> anyhow::Result<()> {
    let env = std::fs::read_to_string(ENV).unwrap_or_default();
    if env.lines().any(|l| {
        l.strip_prefix("TOTP_SECRET=")
            .is_some_and(|v| !v.trim().is_empty())
    }) {
        bail!("TOTP_SECRET is already in {ENV}. Delete that line first to enrol a new device.");
    }

    // Same settings as the bot's security::totp: SHA-1, 6 digits, 30 seconds.
    let secret = Secret::generate();
    let encoded = secret.to_base32();
    let totp = Builder::new()
        .with_secret(secret)
        .with_issuer(Some("Tilly"))
        .with_account_name("owner")
        .build()
        .map_err(|e| anyhow::anyhow!("TOTP: {e:?}"))?;
    let url = totp
        .to_url()
        .map_err(|e| anyhow::anyhow!("TOTP URL: {e:?}"))?;

    let qr = QrCode::new(url).context("QR code")?;
    println!(
        "\nScan this with your authenticator app (1Password, Authy, Google Authenticator…):\n"
    );
    println!("{}", qr.render::<Dense1x2>().quiet_zone(true).build());
    println!("Or enter this key by hand: {encoded}\n");

    let stdin = std::io::stdin();
    for _ in 0..3 {
        print!("Type the 6-digit code it shows: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        if totp.check_current(line.trim()).is_some() {
            save(&env, &encoded)?;
            println!("\nDone. TOTP_SECRET is in {ENV}; restart Tilly to turn codes on.");
            return Ok(());
        }
        println!("That code doesn't match. Check the clock on your phone and try again.");
    }
    bail!("No match after three tries; nothing was saved.")
}

/// Fills an empty `TOTP_SECRET=` line, or appends one.
fn save(env: &str, secret: &str) -> anyhow::Result<()> {
    let line = format!("TOTP_SECRET={secret}");
    let mut found = false;
    let mut out: Vec<String> = env
        .lines()
        .map(|l| {
            if l.starts_with("TOTP_SECRET=") {
                found = true;
                line.clone()
            } else {
                l.to_owned()
            }
        })
        .collect();
    if !found {
        out.push(String::new());
        out.push("# Authenticator codes for loosening (contacts, limits, resume).".into());
        out.push(line);
    }
    std::fs::write(ENV, out.join("\n") + "\n").with_context(|| format!("writing {ENV}"))
}
