# Warden

Codename for **Tilly**: an AI agent that pays for you, within your rules.
The model only proposes payments; deterministic Rust policy checks every one,
and anything above a threshold waits for a human. Spec: [../docs/warden-spec.md](../docs/warden-spec.md).

## Layout

| Path | Crate | Job |
| --- | --- | --- |
| `crates/domain` | `warden-domain` | Types, events, state machines, ports. No I/O. |
| `crates/policy` | `warden-policy` | Rule pipeline: Allow / RequireApproval / Deny |
| `crates/agent/core` | `warden-agent-core` | LLM loop, `Llm` and `Tool` ports |
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

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
