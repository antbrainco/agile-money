# Security

Agile Money moves money, so security reports are the most useful contribution you can make.

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private vulnerability reporting instead: the
repository's **Security** tab, then **Report a vulnerability**. Include what you found, how to
reproduce it, and what an attacker could do with it.

You'll get an acknowledgement within a few days. Fixes land before the report is made public,
and you'll be credited unless you'd rather not be.

## What's in scope

Anything that lets someone pay, or get paid, outside the owner's rules:

- getting a payment past policy, limits, approvals, contacts or the cooling-off period;
- getting the model to act on injected text (from a chat, invoice or web page) in a way policy
  doesn't catch;
- signing in to the dashboard, or linking a wallet or Telegram account, without the owner's
  password, wallet or authenticator code;
- making the vault program (`contracts/sol/vault`) pay beyond the limits the owner signed;
- leaking secrets: bot tokens, wallet keys, API keys, TOTP secrets.

## Running it safely

The project is experimental and unaudited. Defaults are devnet and PayPal sandbox; mainnet needs
an explicit opt-in per rail. If you run it with real funds anyway:

- keep wallet keys in a KMS or HSM rather than environment variables;
- fund the bot only with what you're willing to lose, ideally through the vault;
- turn on authenticator codes (`--example totp_setup`) before adding contacts;
- keep `DASHBOARD_TOKEN`, `DASHBOARD_PASSWORD` and `TOTP_SECRET` out of version control.
