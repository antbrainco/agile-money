# sol/vault

Solana program that holds Tilly's SOL and pays out only within limits the owner
signed for: a cap per payment, a cap per 24-hour window, and a list of up to 16
recipients. The bot's hot key is the vault's *agent*: it can ask for a payment,
but it can't raise a cap, add a recipient or withdraw. Those need the owner key,
which the bot never holds.

So the off-chain policy decides what Tilly *should* pay, and this program bounds
what a stolen bot key *can* pay.

Plain Rust on `solana-program`, no Anchor. Layout and instructions are in the
crate docs at the top of [`src/lib.rs`](src/lib.rs); the client side is
`crates/rail/sol/src/vault.rs`.

```bash
cargo-build-sbf                 # target/deploy/tilly_vault.so
cargo test                      # unit tests, then the built program in litesvm
solana program deploy -ud --keypair <owner.json> target/deploy/tilly_vault.so
```

Then open and fund it with `cargo run -p warden-rail-sol --example vault` from the
repo root, and set `[rail.sol.vault]` in `warden.toml`.

SOL only for now: USDC still pays from the bot's wallet.
