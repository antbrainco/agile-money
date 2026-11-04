# dashboard

Tilly's dashboard: answer approvals, see payments and payment requests, edit the
spending limits, and pause payments. A Vite + React app served by a Cloudflare
Worker (`worker/index.ts`).

- The bot serves a JSON API on `127.0.0.1:8787` when `DASHBOARD_TOKEN` is set
  (`apps/telegram/bot/src/dashboard.rs`).
- The Worker signs the owner in with `DASHBOARD_PASSWORD`, sets a 12-hour signed
  cookie, and forwards `/api/*` to `BOT_API_URL` with the bot's token. The token
  never reaches the browser.
- Approvals go to Telegram and here at once. The first answer wins, and answering
  here closes the Telegram card (rule 8).
- A limits edit is saved as a new policy version marked `dashboard` (rule 9).
  Startup only reseeds from warden.toml when the file changed, so these edits
  survive a restart.

## Run locally

```bash
npm install
cp .dev.vars.example .dev.vars   # same DASHBOARD_TOKEN as the bot's .env; pick a password
npm run dev                      # http://localhost:5173
```

## Deploy

The Worker reaches the bot through the tunnel, so first route `/api/` on the
tunnel's hostname to the API listener in `~/.cloudflared/tilly-dev.yml`, above the
catch-all:

```yaml
  - hostname: bot.example.com
    path: ^/api/
    service: http://127.0.0.1:8787
```

Then:

```bash
npx wrangler secret put DASHBOARD_TOKEN
npx wrangler secret put DASHBOARD_PASSWORD
npm run deploy
```

`BOT_API_URL` in `wrangler.jsonc` points at the tunnel.
