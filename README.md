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
2. Put your DeepSeek key in `.env` as `DEEPSEEK_API_KEY`.
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
