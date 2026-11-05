# Agile Money

[![CI](https://github.com/noelukwa/agile-money/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/noelukwa/agile-money/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

**Open-source payment infrastructure for the agent economy.**

AI agents now do real work: sourcing suppliers, negotiating prices, buying compute and paying
for data. Each of these jobs ends with money changing hands, and Agile Money makes that step
safe. Any agent can pay, get paid and settle in seconds, on crypto or traditional rails, while
its owner stays in control.

Agents move at machine speed, but every payment passes through rules they cannot rewrite:
spending limits, trusted payees, human approval where it matters and on-chain guardrails that
still apply if the server is compromised. The checks run separately from the model, which
limits what a malicious prompt can do.

Every payment also records who requested it, why, which rule allowed it and who approved it.
From that record, Agile Money surfaces spending patterns, cash flow and unusual activity, so
you catch problems before they cost you.

The aim is payment infrastructure that keeps pace with agents, keeps their owners in control
and is open for anyone to build on.

## How it works

1. **Ask.** An agent, a person or another service asks to pay or to be paid.
2. **Check.** The request becomes a proposal, and your rules allow it, hold it for approval,
   or block it with a reason.
3. **Settle.** A small, isolated signer, the only part that can touch funds, executes it on
   the right network and records the receipt.

```
request ──▶ agent (proposes) ──▶ rules (allow / ask / deny) ──▶ you (approve)
                                                                  │
                                       receipt ◀── signer ◀───────┘
```

It runs on Solana devnet and PayPal sandbox by default. See the [roadmap](#features-and-roadmap)
for what's built and what's next.

---

## Quick start

You need the Rust toolchain (version pinned in `rust-toolchain.toml`), Docker, and
[sqlx-cli](https://crates.io/crates/sqlx-cli) 0.9.

```bash
cp .env.example .env
docker compose up -d --wait postgres
sqlx migrate run
cargo test --workspace --all-features
```

Then [run it on Telegram](#run-it-on-telegram-solana-devnet), or try only the
[PayPal flow](#try-the-paypal-flow).

## Try the PayPal flow

The shortest path to seeing the agent take a payment by PayPal, all in the PayPal sandbox. It
takes about 15 minutes and needs no crypto: the bot wants a Solana key to start, but it never
has to be funded for this.

You need the [quick start](#quick-start) tools, a Telegram account, and:

- **A PayPal sandbox app.** At https://developer.paypal.com, sign in and open
  *Apps & Credentials* (Sandbox), then create a REST app. Copy its client ID and secret.
- **A sandbox buyer.** Under *Testing Tools → Sandbox Accounts*, PayPal creates a Personal
  (buyer) account for you. Note its email and password: you pay the checkout link with it.
- **A Telegram bot.** Message [@BotFather](https://t.me/BotFather), send `/newbot`, and copy
  the token.
- **A model key**, e.g. a DeepSeek API key, or another of the [supported providers](#llm-providers).

Then:

1. Start Postgres and make a devnet key (it prints a line for `.env`; skip the faucet):

   ```bash
   cp .env.example .env
   docker compose up -d --wait postgres
   cargo run -p agile-money-rail-sol --example keygen
   ```

2. In `.env`, fill in `TELOXIDE_TOKEN`, `DEEPSEEK_API_KEY`, the `AGILE_MONEY_RAIL__SOL__KEYPAIR`
   line from step 1, and uncomment the two PayPal lines with your sandbox client ID and
   secret.
3. Start the bot and send it `/start` in Telegram. It replies with your chat id; set it as
   `AGILE_MONEY_TELEGRAM__OWNER_CHAT_ID` in `.env`, then stop the bot and start it again. The
   log should say `PayPal requests on`.

   ```bash
   set -a; . ./.env; set +a
   cargo run -p agile-money-telegram-bot
   ```

4. Ask the bot: **"request 20 USD from Dana for the logo"**. It replies with a PayPal checkout
   link and a QR code.
5. Open the link in a browser and pay with the sandbox **buyer** account (or a sandbox card).
6. Within about 10 seconds the bot captures the order and tells you in Telegram that Dana has
   paid. If you cancel the checkout instead, or it expires, the bot tells you it was cancelled.

To see the request in the dashboard's history as well, follow step 8 of the next section.

## Run it on Telegram (Solana devnet)

1. Create a bot with @BotFather and put its token in `.env` as `TELOXIDE_TOKEN`.
2. Give it a model: put a key from one of the [supported providers](#llm-providers) in
   `.env`, e.g. `DEEPSEEK_API_KEY`.
3. Make a devnet wallet, paste the printed line into `.env`, and fund the address at
   https://faucet.solana.com:

   ```bash
   cargo run -p agile-money-rail-sol --example keygen
   ```

4. Start the bot and send it `/start`. It replies with your chat id. Set that id as
   `AGILE_MONEY_TELEGRAM__OWNER_CHAT_ID` in `.env` and restart.

   ```bash
   cargo run -p agile-money-telegram-bot
   ```

5. Add `[[contacts]]` to `agile-money.local.toml` and adjust the limits in `[policy.sol]` and
   `[policy.usdc]`. Then chat: "pay Bob 0.01 SOL for lunch". Payments above `approval_above`
   wait for your Approve tap.
   - For USDC, get devnet USDC at https://faucet.circle.com. The first USDC payment to a
     contact also opens their token account, about 0.002 SOL paid by your wallet.
   - Remove `[policy.usdc]` to turn USDC off: an asset without limits can't be paid.
6. To get paid, ask: "request 5 USDC from Dana for the website". You get a Solana Pay QR code
   and link to forward; any Solana Pay wallet (Phantom, Solflare) can pay it. When the money
   arrives, the bot tells you. Spending limits don't apply to money coming in.
7. To take USD by PayPal, create a sandbox REST app at https://developer.paypal.com and set
   `AGILE_MONEY_RAIL__PAYPAL__CLIENT_ID` and `AGILE_MONEY_RAIL__PAYPAL__CLIENT_SECRET`. Then ask:
   "request 20 USD from Dana for the logo". Your client pays by PayPal or card, and the bot
   tells you once it's captured.
8. For the dashboard, set `DASHBOARD_TOKEN` in `.env` (`openssl rand -hex 32`). In
   `apps/dashboard`, copy `.dev.vars.example` to `.dev.vars` with the same token and a
   password, then run `npm install && npm run dev` and open http://localhost:5173. Deploying
   it is covered in [apps/dashboard/README.md](apps/dashboard/README.md).

By default the bot long-polls Telegram. Set `telegram.webhook.url` (or `TUNNEL_URL`) to a
public HTTPS URL, for example a Cloudflare Tunnel, and it takes updates there instead. Remove
the URL to go back to polling.

## LLM providers

| Provider | `agent.provider` | Key in `.env` | Default model |
| --- | --- | --- | --- |
| DeepSeek | `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-flash` |
| Anthropic Claude | `claude` | `ANTHROPIC_API_KEY` | `claude-sonnet-5-5` |
| OpenAI | `openai` | `OPENAI_API_KEY` | `gpt-5.5` |
| Any OpenAI-compatible API | `compatible` | `AGILE_MONEY_AGENT__API_KEY` (if needed) | none: set `model` |

`compatible` covers Cloudflare Workers AI, OpenRouter, Ollama, vLLM and anything else that
speaks the OpenAI Chat Completions format. Give it a `base_url` and a `model`:

```toml
# agile-money.local.toml
[agent]
provider = "compatible"
base_url = "http://localhost:11434/v1/"   # Ollama
model = "qwen3"
```

**Picking a provider.** Set `agent.provider` to choose one. If you leave it unset, it's
detected when the bot starts: a `base_url` means `compatible`; otherwise the first key found
among `DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` wins. The startup log says
which provider is active and why.

**Other settings** live under `[agent]` (see `agile-money.toml`): `model`, `stream` (replies
appear in Telegram as they're written), `max_tokens`, timeouts and retries under
`[agent.http]`, and per-provider options under `[agent.openai]` and `[agent.anthropic]`.

## Cloudflare AI Gateway

[AI Gateway](https://developers.cloudflare.com/ai-gateway/) sits between the bot and your
provider, adding logs, caching, rate limits and spend tracking. It works with every provider
above.

1. In the Cloudflare dashboard, open **AI > AI Gateway** and create a gateway. Note its name
   (the gateway id) and your account id.
2. Optional but recommended: turn on **Authenticated Gateway** and create a token. Put it in
   `.env` as `AGILE_MONEY_AGENT__GATEWAY__TOKEN`; it's sent as `cf-aig-authorization`.
3. Point the bot at it in `agile-money.local.toml`. With a gateway, `provider` must be set:

   ```toml
   [agent]
   provider = "claude"            # deepseek | claude | openai | compatible

   [agent.gateway]
   account_id = "<ACCOUNT_ID>"
   gateway_id = "my-gateway"
   ```

4. Keep the provider's key in `.env` as usual, or store it in the gateway (**Provider keys**
   in the gateway's settings) and leave it out of `.env` entirely.

Requests then go to `https://gateway.ai.cloudflare.com/v1/<account>/<gateway>/<path>/`:

| `agent.provider` | Gateway path | Model example |
| --- | --- | --- |
| `deepseek` | `deepseek` | `deepseek-flash` |
| `claude` | `anthropic` | `claude-sonnet-5-5` |
| `openai` | `openai` | `gpt-5.5` |
| `compatible` | `compat` | `deepseek/deepseek-flash`, `openai/gpt-5.5` |

With `compatible`, the gateway's unified endpoint takes models as `provider/model`, so you
can switch providers by changing only `model`. `gateway` and `base_url` can't be used together.

### Workers AI

To run a model on Cloudflare itself, use Workers AI's OpenAI-compatible API. The key is a
Cloudflare API token with Workers AI permission, set as `AGILE_MONEY_AGENT__API_KEY`:

```toml
[agent]
provider = "compatible"
base_url = "https://api.cloudflare.com/client/v4/accounts/<ACCOUNT_ID>/ai/v1/"
model = "@cf/deepseek-ai/deepseek-v4-flash-0731"

[agent.headers]
cf-aig-gateway-id = "my-gateway"   # optional: also log these calls in your gateway
```

DeepSeek V4 Flash handles tool calls well at about 1.6 seconds a step. Third-party models
are named `author/model`, e.g. `openai/gpt-5.5`, and are billed through Cloudflare.

## Configuration

Settings are layered, each overriding the last:

1. `agile-money.toml`: shared defaults with commented examples. Keep it generic.
2. `agile-money.local.toml`: your own deployment, git-ignored and optional. Put your webhook
   URL, model, vault and contacts here.
3. Environment variables `AGILE_MONEY_*`, with `__` for nesting, e.g.
   `AGILE_MONEY_TELEGRAM__OWNER_CHAT_ID`.

Secrets (bot token, API keys, wallet key, dashboard and TOTP secrets) only ever come from the
environment, `.env` locally, never from either file.

## Deploy to a server

The bot runs as one container next to a Cloudflare Tunnel container, which carries Telegram's
webhook in, so the server needs no open ports. Postgres is external, e.g. Neon's free tier;
use its direct endpoint, not the `-pooler` one.

1. Create a Neon project and copy its direct connection string.
2. On the server, clone the repo and go to `deploy/`. Copy `bot.env.example` to `bot.env`,
   fill it in, and run `chmod 600 bot.env`.
3. Copy `cloudflared.example.yml` to `cloudflared.yml` with your tunnel id and hostname, and
   copy the tunnel's credentials file to `deploy/tunnel.json`.
4. Start it:

   ```bash
   docker compose up -d --build
   ```

Migrations run on start. The wallet key comes from the environment, which is fine for
devnet; with real funds, keep keys in a KMS or HSM.

## Project layout

| Path | What's there |
| --- | --- |
| `crates/domain` | Payments, events and their states |
| `crates/policy` | The rules: allow, ask or deny |
| `crates/agent/core` | The agent loop and the LLM client |
| `crates/executor` | The only code that signs |
| `crates/store` | Postgres event log |
| `crates/rail/sol`, `crates/rail/paypal` | Solana and PayPal |
| `crates/rail/{near,bch,concordium}`, `crates/scout`, `crates/x402` | Placeholders for later |
| `apps/telegram/bot` | The Telegram bot and the dashboard's API |
| `apps/dashboard` | The web dashboard, on a Cloudflare Worker |
| `apps/shop` | Instagram and WhatsApp webhooks (early) |
| `contracts/sol/vault` | Solana program that bounds what the bot can pay out |

A few internal names still say `tilly`, the project's first name: the vault program, its
account tag and some derived ids. Renaming them would orphan existing on-chain and database
data.

## Features and roadmap

### Payments
- [x] Payouts in SOL and USDC on Solana
- [x] Payment requests as Solana Pay QR codes and links
- [x] USD collection through PayPal and card checkout
- [x] Plain-language payments and requests through the agent
- [ ] Micropayments and streaming payments for pay-per-use services
- [ ] Agent-to-agent payments over HTTP 402 (x402)
- [ ] Recurring payments and subscriptions
- [ ] Cross-currency settlement and stablecoin routing
- [ ] More networks: NEAR, Bitcoin Cash, Concordium, EVM chains

### Security and control
- [x] Per-payment and daily limits for each asset
- [x] Human approval above a threshold, in Telegram or on the web
- [x] Trusted payees, with an authenticator code and a cooling-off period for new ones
- [x] On-chain spending vault that enforces owner-signed limits
- [x] Kill switch that pauses every payment and survives restarts
- [x] No double payments: idempotent execution and crash recovery
- [ ] Fraud and anomaly detection: unusual amounts, new-payee patterns, velocity checks
- [ ] Risk scoring for every payment
- [ ] Team accounts with roles and multi-party approval
- [ ] Hardware and cloud key management (HSM, KMS)
- [ ] Independent security audit of the bot and the vault

### Insight
- [x] Full decision log: what was asked, which rule decided, who approved, the on-chain receipt
- [x] Dashboard for approvals, history, requests, contacts and limits
- [ ] Spend analytics: trends, categories and top payees
- [ ] Cash-flow forecasting and budget alerts
- [ ] Accounting exports and automatic reconciliation

### Agents and integrations
- [x] Bring your own model: DeepSeek, Claude, OpenAI, Workers AI or any OpenAI-compatible API
- [x] Cloudflare AI Gateway support
- [x] Telegram interface
- [x] Web dashboard with Solana wallet sign-in
- [ ] Public API and SDKs for embedding payments in any agent
- [ ] MCP server, so any MCP-capable agent can pay through Agile Money
- [ ] WhatsApp, Instagram and Slack channels
- [ ] Supplier and invoice verification from the web before paying

Want to help build any of these? See [CONTRIBUTING.md](CONTRIBUTING.md).

## Linting and tests

Every push to `main` and every pull request runs the checks below in
[CI](.github/workflows/ci.yml). The badge at the top shows their status on `main`.

| Area | Lint | Tests |
| --- | --- | --- |
| Services (`crates/`, `apps/telegram`, `apps/shop`) | `cargo fmt`, `cargo clippy` with warnings as errors | Unit and integration tests, including the event store against a real Postgres |
| Dependencies | `cargo deny`: licences, security advisories, banned crates and sources | |
| Dashboard (`apps/dashboard`) | ESLint, TypeScript type check | Worker tests: sign-in, rate limits, sessions and the API proxy |
| Vault (`contracts/sol/vault`) | `cargo fmt`, `cargo clippy` | Unit tests, then the built program in a local Solana VM |

Run them locally:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets
cargo test --workspace --all-features
```

```bash
cd apps/dashboard && npm run lint && npm test && npm run build
```

```bash
cd contracts/sol/vault && cargo-build-sbf && cargo test
```

## Status

Experimental. It runs on Solana devnet and PayPal sandbox by default; mainnet needs an
explicit opt-in per network. Neither the bot nor the vault has been audited, so don't point it
at money you can't afford to lose.

## Contributing and security

Development setup and checks are in [CONTRIBUTING.md](CONTRIBUTING.md). Please report
vulnerabilities privately, as described in [SECURITY.md](SECURITY.md), not in public issues.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
