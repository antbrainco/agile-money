# Warden

Codename for **Tilly**: an AI agent that pays for you, within your rules.
The model only proposes payments; deterministic Rust policy checks every one,
and anything above a threshold waits for a human. Spec: [../docs/warden-spec.md](../docs/warden-spec.md).

## Layout

| Path | Crate | Job |
| --- | --- | --- |
| `crates/domain` | `warden-domain` | Types, events, state machines, ports. No I/O. |
| `crates/policy` | `warden-policy` | Rule pipeline: Allow / RequireApproval / Deny |
| `crates/agent/core` | `warden-agent-core` | Agent loop, `Llm` and `Tool` ports, streaming LLM client |
| `crates/store` | `warden-store` | Postgres event log + outbox (sqlx) |
| `crates/executor` | `warden-executor` | The only code that signs |
| `crates/rail/*` | `warden-rail-*` | One crate per network |
| `crates/scout` | `warden-scout` | Tavily search via Bifrost |
| `crates/x402` | `warden-x402` | HTTP 402 challenges and proofs |
| `apps/telegram/bot` | `warden-telegram-bot` | Tilly on Telegram |
| `apps/shop` | `warden-shop` | Instagram/WhatsApp webhooks |
| `apps/dashboard` | (npm) | Approvals, history and limits; Vite + React on a Cloudflare Worker |

`apps/dashboard`, `contracts/*` and `services/*` build with their own toolchains and
sit outside the Cargo workspace.

## Develop

```bash
cp .env.example .env
docker compose up -d --wait postgres   # host port 5433
sqlx migrate run                       # sqlx-cli 0.9 with the postgres feature
cargo test --workspace --all-features
```

Run `sqlx migrate run` before the first build against a fresh database: the store's
checked queries need the tables to exist (or build with `SQLX_OFFLINE=true` from `.sqlx/`).
Once built, `cargo run -p warden-store --bin warden-migrate` applies migrations too.

Apps also apply pending migrations on startup (`database.migrate_on_start` in `warden.toml`).
After changing a query or migration, refresh the offline cache and commit `.sqlx/`:

```bash
cargo sqlx prepare --workspace -- --all-targets --all-features
```

Store tests use `#[sqlx::test]`, which creates a throwaway database per test from `DATABASE_URL`.

Apps build one rail per Cargo feature, e.g. `cargo run -p warden-shop --no-default-features --features paypal`.

## Run the Telegram demo (Solana devnet)

1. Create a bot with @BotFather and put its token in `.env` as `TELOXIDE_TOKEN`.
2. `warden.toml` uses DeepSeek V4 Flash on Cloudflare Workers AI. Put a Cloudflare API token
   with Workers AI permission in `.env` as `WARDEN_AGENT__API_KEY`. For other providers, see
   [Model providers](#model-providers).
3. Make a devnet wallet and paste the printed line into `.env`. Then fund the address at
   https://faucet.solana.com:

   ```bash
   cargo run -p warden-rail-sol --example keygen
   ```

4. Start the bot and send it `/start`. It replies with your chat id. Set that id as
   `WARDEN_TELEGRAM__OWNER_CHAT_ID` and restart.
5. Edit `[[contacts]]` and the per-asset limits in `[policy.sol]` and `[policy.usdc]` in
   `warden.toml`, then chat: "pay Bob 0.01 SOL for lunch" or "pay Acme Design 5 USDC".
   Payments above `approval_above` wait for your Approve tap. Each confirmed payment links to the
   devnet explorer. For USDC, get devnet USDC for the wallet at https://faucet.circle.com
   (Solana Devnet). Drop `[policy.usdc]` to turn USDC off; an asset without limits can't be paid.
   The first USDC payment to a contact also creates their token account, about 0.002 SOL of rent
   paid by the wallet.
6. To get paid, ask: "request 5 USDC from Dana for the website". Tilly records an invoice and
   sends you a Solana Pay QR code and link to forward. Any Solana Pay wallet (Phantom, Solflare)
   can pay it. A watcher checks open requests every 10 seconds for a week, using the request's
   `reference`. When the total arrives it records `invoice_paid` and tells you, with an
   explorer link. No spending policy applies to money coming in.
7. To take requests in USD by PayPal, create a REST app at https://developer.paypal.com
   (sandbox) and set `WARDEN_RAIL__PAYPAL__CLIENT_ID` and `WARDEN_RAIL__PAYPAL__CLIENT_SECRET`.
   Then ask: "request 20 USD from Dana for the logo". Tilly creates a PayPal order and sends
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
cargo run -p warden-telegram-bot
```

By default the bot long-polls Telegram. When `telegram.webhook.url` (or `TUNNEL_URL`) is set it
takes updates on that public HTTPS URL instead. On every start it registers the URL and the
`TELEGRAM_WEBHOOK_SECRET` with Telegram and checks what Telegram stored. The webhook stays set
while the bot is down, so updates queue until it is back. Without a secret it uses a random one
per run. Locally, a Cloudflare Tunnel forwards the URL to the bot's listener:

```bash
cloudflared tunnel --config ~/.cloudflared/tilly-dev.yml run
```

Remove the URL to go back to polling; polling mode deletes the webhook on start.

The rail's live test sends 1000 lamports to its own wallet and checks that a retry doesn't pay twice:

```bash
cargo test -p warden-rail-sol --features live --test live -- --nocapture
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
3. Copy the tunnel's credentials file, `~/.cloudflared/<tunnel-id>.json`, to `deploy/tunnel.json`.
4. Stop the laptop's tunnel and bot, then start the server's:

   ```bash
   docker compose up -d --build
   docker compose logs -f bot
   ```

Migrations run on start. The image bakes in `warden.toml` and nothing secret; `WARDEN_*`
variables in `bot.env` override it. The wallet key comes from env, which is fine for devnet;
with real funds, keep keys in a KMS or HSM.

## Model providers

`agent.provider` in `warden.toml` names the provider: `deepseek`, `claude`, `openai`, or
`compatible` for any OpenAI-style endpoint (set `base_url` and `model`). Leave it unset and
it is detected at startup:

1. A `base_url` means `compatible`.
2. Otherwise the first key variable set among `DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY` and
   `OPENAI_API_KEY` picks `deepseek`, `claude` or `openai`.

The startup log lists the active provider, why it was chosen, and every provider whose
key is present. A configured `provider` always wins. Its key comes from its own variable
or `WARDEN_AGENT__API_KEY`. A gateway or a bare `WARDEN_AGENT__API_KEY` cannot say which
API to speak, so those need `provider` set.

To go through Cloudflare AI Gateway, add `[agent.gateway]` with your `account_id` and
`gateway_id`. Calls then go to `gateway.ai.cloudflare.com/v1/{account}/{gateway}/{provider}/`.
For an authenticated gateway, set `WARDEN_AGENT__GATEWAY__TOKEN` (sent as
`cf-aig-authorization`). If the gateway stores your provider key, leave the API key unset.
With `provider = "compatible"`, the gateway's `compat` endpoint is used; give models as
`provider/model`, e.g. `deepseek/deepseek-flash`.

The client lives in `crates/agent/core/src/llm`. A `RoundTripper` stack (headers, retries,
tracing) carries the HTTP; a `Wire` per API format builds bodies and decodes replies, whole or
streamed over SSE.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
