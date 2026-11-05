# Contributing

Thanks for helping. Bug reports, fixes, new rails and docs are all welcome. For anything
security-related, follow [SECURITY.md](SECURITY.md) instead of opening an issue.

## Getting set up

Follow the [quick start](README.md#quick-start): Rust (pinned in `rust-toolchain.toml`),
Docker for Postgres, and sqlx-cli 0.9. The dashboard in `apps/dashboard` needs Node 22 or later.

Put your own settings in `agile-money.local.toml` and secrets in `.env`; both are git-ignored.
Keep `agile-money.toml` generic.

## Before you open a pull request

CI runs these, so run them first:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets
cargo test --workspace --all-features
```

If you changed a SQL query or migration, refresh the offline query cache and commit `.sqlx/`:

```bash
cargo sqlx prepare --workspace -- --all-targets --all-features
```

For the dashboard:

```bash
cd apps/dashboard && npm ci && npm run build
```

## How the code is meant to stay

- **The model proposes; policy decides.** Nothing the model says may raise a limit, add a payee
  or skip an approval. Treat chat text, invoices and web content as untrusted data.
- **Only the executor signs.** New rails implement the `Rail` port in `crates/domain` and are
  driven by the executor, never called from the agent.
- **Loosening needs a code; tightening never does.** Anything that widens what can be paid
  (new contacts, higher limits, new sign-in methods) asks for the authenticator code once codes
  are on, and tells the owner on Telegram.
- **No secrets in files.** Secrets come from the environment only.
- **No `unwrap`/`expect` outside tests**; clippy denies them.

Keep pull requests focused, with a clear description of what changed and why. Commit messages
are plain sentences in the imperative ("Add the NEAR rail"), with a body when the why isn't
obvious.

By contributing you agree that your work is dual-licensed under MIT and Apache-2.0, like the
rest of the project.
