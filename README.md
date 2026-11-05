# Agile Money

An AI agent that pays people for you and gets you paid, within rules you set.

You chat with it on Telegram: "pay Acme Design 5 USDC for the logo", or "request 20 USD
from Dana for the website". The language model only *proposes* payments. Deterministic
Rust policy checks every one, anything above your threshold waits for your tap, and the only
code that signs is a small executor that acts on intents that passed policy.

## What it does

- **Pays on Solana** in SOL and USDC (devnet by default), with an explorer link for every
  confirmed payment. A retry never pays twice.
- **Gets you paid** with Solana Pay links and QR codes, or PayPal checkout links for USD. A
  watcher notices when the money arrives and tells you.
- **Asks before it pays** anything above `approval_above`, in Telegram or on the dashboard.
  The first answer wins and closes the other.
- **Only pays your contacts.** A new or changed address needs an authenticator code once
  codes are on, then waits out a cooling-off period.
- **Caps spending** per payment and per day, per asset. `/pause` stops all payments and
  survives restarts.
- **Can hold funds in an on-chain vault** ([contracts/sol/vault](contracts/sol/vault)) that
  enforces owner-signed limits even if the bot's key leaks.
- **Has a web dashboard** for approvals, history, payment requests, contacts, limits and
  security. You sign in with a password or a linked Solana wallet, plus your authenticator
  code, and confirm your Telegram account from the Profile page.

## How it fits together

```
Telegram / dashboard ──▶ agent (LLM proposes) ──▶ policy (allow / ask / deny)
                                                     │
                                  owner approves ◀───┤
                                                     ▼
                               executor (signs) ──▶ rail (Solana, PayPal)
                                     │
                              Postgres event log + outbox
```

Every step is an event in Postgres, so a crash mid-payment resumes instead of paying twice.

## Layout

| Path | Crate | Job |
| --- | --- | --- |
| `crates/domain` | `agile-money-domain` | Types, events, state machines, ports. No I/O. |
| `crates/policy` | `agile-money-policy` | Rule pipeline: Allow / RequireApproval / Deny |
| `crates/agent/core` | `agile-money-agent-core` | Agent loop, `Llm` and `Tool` ports, streaming LLM client |
| `crates/store` | `agile-money-store` | Postgres event log + outbox (sqlx) |
| `crates/executor` | `agile-money-executor` | The only code that signs |
| `crates/rail/sol` | `agile-money-rail-sol` | Solana: SOL, USDC, Solana Pay, the vault |
| `crates/rail/paypal` | `agile-money-rail-paypal` | PayPal orders for incoming USD |
| `crates/rail/{near,bch,concordium}` | `agile-money-rail-*` | Placeholders for more networks |
| `crates/scout`, `crates/x402` | `agile-money-scout`, `agile-money-x402` | Placeholders: web search, HTTP 402 payments |
| `apps/telegram/bot` | `agile-money-telegram-bot` | The Telegram bot and the dashboard's API |
| `apps/shop` | `agile-money-shop` | Instagram/WhatsApp webhooks (early) |
| `apps/dashboard` | (npm) | Vite + React on a Cloudflare Worker |
| `contracts/sol/vault` | `tilly-vault` | Solana program that bounds what the bot can pay out |

`apps/dashboard` and `contracts/sol/vault` build with their own toolchains and
sit outside the Cargo workspace. A few names still say `tilly`, the project's first name:
the vault program, its account tag and some derived ids. Changing them would orphan data
that already exists, on chain or in the database.

## Quick start

You need Rust (pinned in `rust-toolchain.toml`), Docker, and
[sqlx-cli](https://crates.io/crates/sqlx-cli) 0.9 with the postgres feature.

```bash
cp .env.example .env
docker compose up -d --wait postgres
sqlx migrate run
cargo test --workspace --all-features
```

Then follow [Run the Telegram demo](#run-the-telegram-demo-solana-devnet) to talk to it.

## Configuration

Settings are layered, each overriding the last:

1. `agile-money.toml`: shared defaults and documented examples. Keep it generic.
2. `agile-money.local.toml`: your own deployment, git-ignored and optional, same shape. Put
   your webhook URL, model endpoint, vault and contacts here.
3. `AGILE_MONEY_*` environment variables, with `__` for nesting, e.g.
   `AGILE_MONEY_TELEGRAM__OWNER_CHAT_ID`. Also `DATABASE_URL`, `TELOXIDE_TOKEN`, `TUNNEL_URL`,
   `TELEGRAM_WEBHOOK_SECRET`, `DASHBOARD_TOKEN` and `TOTP_SECRET`.

Secrets only ever come from the environment (`.env` locally), never from either file.

## Develop

Postgres listens on host port 5433. Run `sqlx migrate run` before the first build against a fresh database: the store's
checked queries need the tables to exist (or build with `SQLX_OFFLINE=true` from `.sqlx/`).
Once built, `cargo run -p agile-money-store --bin agile-money-migrate` applies migrations too.

Apps also apply pending migrations on startup (`database.migrate_on_start` in `agile-money.toml`).
After changing a query or migration, refresh the offline cache and commit `.sqlx/`:

```bash
cargo sqlx prepare --workspace -- --all-targets --all-features
```

Store tests use `#[sqlx::test]`, which creates a throwaway database per test from `DATABASE_URL`.

Apps build one rail per Cargo feature, e.g. `cargo run -p agile-money-shop --no-default-features --features paypal`.

## Run the Telegram demo (Solana devnet)

1. Create a bot with @BotFather and put its token in `.env` as `TELOXIDE_TOKEN`.
2. Give it a model: put `DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` in `.env`,
   and the provider is picked from whichever is set. `agile-money.toml` also shows how to use
   DeepSeek V4 Flash on Cloudflare Workers AI. See [Model providers](#model-providers).
3. Make a devnet wallet and paste the printed line into `.env`. Then fund the address at
   https://faucet.solana.com:

   ```bash
   cargo run -p agile-money-rail-sol --example keygen
   ```

4. Start the bot and send it `/start`. It replies with your chat id. Set that id as
   `AGILE_MONEY_TELEGRAM__OWNER_CHAT_ID` and restart.
5. Add `[[contacts]]` in `agile-money.local.toml` (see [Configuration](#configuration)) and
   adjust the per-asset limits in `[policy.sol]` and `[policy.usdc]`, then chat: "pay Bob 0.01 SOL for lunch" or "pay Acme Design 5 USDC".
   Payments above `approval_above` wait for your Approve tap. Each confirmed payment links to the
   devnet explorer. For USDC, get devnet USDC for the wallet at https://faucet.circle.com
   (Solana Devnet). Drop `[policy.usdc]` to turn USDC off; an asset without limits can't be paid.
   The first USDC payment to a contact also creates their token account, about 0.002 SOL of rent
   paid by the wallet.
6. To get paid, ask: "request 5 USDC from Dana for the website". Agile Money records an invoice and
   sends you a Solana Pay QR code and link to forward. Any Solana Pay wallet (Phantom, Solflare)
   can pay it. A watcher checks open requests every 10 seconds for a week, using the request's
   `reference`. When the total arrives it records `invoice_paid` and tells you, with an
   explorer link. No spending policy applies to money coming in.
7. To take requests in USD by PayPal, create a REST app at https://developer.paypal.com
   (sandbox) and set `AGILE_MONEY_RAIL__PAYPAL__CLIENT_ID` and `AGILE_MONEY_RAIL__PAYPAL__CLIENT_SECRET`.
   Then ask: "request 20 USD from Dana for the logo". Agile Money creates a PayPal order and sends
   you its checkout link and QR code; the client pays by PayPal or card. The watcher captures
   the order once the client approves it, then tells you it's paid. Every PayPal write
   carries a `PayPal-Request-Id`, so a retry never charges twice. Pay sandbox orders with a
   sandbox personal account from the developer dashboard.
8. For the dashboard, set `DASHBOARD_TOKEN` in `.env` (`openssl rand -hex 32`); the bot then
   serves its API on `127.0.0.1:8787`. In `apps/dashboard`, copy `.dev.vars.example` to
   `.dev.vars` with the same token and a password, then `npm install && npm run dev` and open
   http://localhost:5173. Approve or reject there or in Telegram: the first answer wins and
   closes the other. Limits edits are saved as new policy versions and survive restarts.
   Deploying the Worker is in [apps/dashboard/README.md](apps/dashboard/README.md).

```bash
cargo run -p agile-money-telegram-bot
```

By default the bot long-polls Telegram. When `telegram.webhook.url` (or `TUNNEL_URL`) is set it
takes updates on that public HTTPS URL instead. On every start it registers the URL and the
`TELEGRAM_WEBHOOK_SECRET` with Telegram and checks what Telegram stored. The webhook stays set
while the bot is down, so updates queue until it is back. Without a secret it uses a random one
per run. Locally, a Cloudflare Tunnel forwards the URL to the bot's listener:

```bash
cloudflared tunnel --config ~/.cloudflared/<your-tunnel>.yml run
```

Remove the URL to go back to polling; polling mode deletes the webhook on start.

The rail's live test sends 1000 lamports to its own wallet and checks that a retry doesn't pay twice:

```bash
cargo test -p agile-money-rail-sol --features live --test live -- --nocapture
```

## Deploy to a server

The bot runs as one container next to a Cloudflare Tunnel container, which carries
Telegram's webhook in, so the server needs no open ports. Postgres is external, e.g. Neon's
free tier. Use Neon's direct endpoint, not the `-pooler` one: the bot sets
`statement_timeout` as a startup option, and migrations take an advisory lock, and PgBouncer
supports neither.

1. Create a Neon project and copy its direct connection string.
2. On the server, clone the repo and go to `deploy/`. Copy `bot.env.example` to `bot.env`,
   fill it in, then run `chmod 600 bot.env`.
3. Copy `cloudflared.example.yml` to `cloudflared.yml` and fill in your tunnel id and
   hostname. Copy the tunnel's credentials file, `~/.cloudflared/<tunnel-id>.json`, to
   `deploy/tunnel.json`.
4. Stop the laptop's tunnel and bot, then start the server's:

   ```bash
   docker compose up -d --build
   docker compose logs -f bot
   ```

Migrations run on start. The image bakes in `agile-money.toml` (and `agile-money.local.toml`
when present) and nothing secret; `AGILE_MONEY_*` variables in `bot.env` override them. The
wallet key comes from env, which is fine for devnet; with real funds, keep keys in a KMS or HSM.

## Model providers

`agent.provider` in `agile-money.toml` names the provider: `deepseek`, `claude`, `openai`, or
`compatible` for any OpenAI-style endpoint (set `base_url` and `model`). Leave it unset and
it is detected at startup:

1. A `base_url` means `compatible`.
2. Otherwise the first key variable set among `DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY` and
   `OPENAI_API_KEY` picks `deepseek`, `claude` or `openai`.

The startup log lists the active provider, why it was chosen, and every provider whose
key is present. A configured `provider` always wins. Its key comes from its own variable
or `AGILE_MONEY_AGENT__API_KEY`. A gateway or a bare `AGILE_MONEY_AGENT__API_KEY` cannot say which
API to speak, so those need `provider` set.

To go through Cloudflare AI Gateway, add `[agent.gateway]` with your `account_id` and
`gateway_id`. Calls then go to `gateway.ai.cloudflare.com/v1/{account}/{gateway}/{provider}/`.
For an authenticated gateway, set `AGILE_MONEY_AGENT__GATEWAY__TOKEN` (sent as
`cf-aig-authorization`). If the gateway stores your provider key, leave the API key unset.
With `provider = "compatible"`, the gateway's `compat` endpoint is used; give models as
`provider/model`, e.g. `deepseek/deepseek-flash`.

The client lives in `crates/agent/core/src/llm`. A `RoundTripper` stack (headers, retries,
tracing) carries the HTTP; a `Wire` per API format builds bodies and decodes replies, whole or
streamed over SSE.

## Status

Experimental. It runs on Solana devnet and PayPal sandbox by default, and mainnet needs an
explicit opt-in per rail. Neither the bot nor the vault program has been audited; don't
point it at money you can't afford to lose.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report vulnerabilities privately, as described
in [SECURITY.md](SECURITY.md), not in public issues.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
