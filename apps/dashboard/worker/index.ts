// The dashboard's Worker: the owner signs in with a password, or a linked Solana
// wallet's signature, and, once codes are set up, an authenticator code the bot
// checks. Then `/api/*` is forwarded to the bot's API with the bot's token. The
// token never reaches the browser; the browser holds only a signed, short-lived
// session cookie.

import { zValidator } from "@hono/zod-validator";
import { Hono, type Context, type MiddlewareHandler } from "hono";
import { deleteCookie, getSignedCookie, setSignedCookie } from "hono/cookie";
import { HTTPException } from "hono/http-exception";
import { proxy } from "hono/proxy";
import { timingSafeEqual } from "hono/utils/buffer";
import { z } from "zod";

type App = { Bindings: Env };
type Ctx = Context<App>;

const COOKIE = "agile_money_session";
/** Sessions last 12 hours. */
const SESSION_SECONDS = 12 * 60 * 60;
const NO_STORE = { "content-type": "application/json", "cache-control": "no-store" };

// Request bodies. Anything else is a 400 before the bot is asked.
const login = z.object({
  // Copying from a file often brings a trailing newline or space along.
  password: z.string().trim(),
  code: z.string().optional(),
});
const challenge = z.object({ address: z.string() });
const walletLogin = z.object({
  address: z.string(),
  signature: z.string(),
  code: z.string().optional(),
});

const body = <T extends z.ZodType>(schema: T) =>
  zValidator("json", schema, (result, c) => {
    if (!result.success) return c.json({ error: "That request isn't valid." }, 400);
  });

/** HMAC key for the session cookie. Derived from the bot token, so rotating it signs everyone out. */
const secret = (c: Ctx) => `session:${c.env.DASHBOARD_TOKEN}`;

/** Slows guessing. Approximate and per location; the bot's code check behind it is
 * strict (five wrong codes lock codes for 15 minutes). */
const rateLimited: MiddlewareHandler<App> = async (c, next) => {
  const ip = c.req.header("cf-connecting-ip") ?? "local";
  const [perIp, overall] = await Promise.all([
    c.env.LOGIN_LIMIT.limit({ key: `ip:${ip}` }),
    c.env.LOGIN_LIMIT_ALL.limit({ key: "all" }),
  ]);
  if (!perIp.success || !overall.success) {
    return c.json({ error: "Too many sign-in attempts. Wait a minute." }, 429);
  }
  await next();
};

const signedIn: MiddlewareHandler<App> = async (c, next) => {
  if (!(await hasSession(c))) return c.json({ error: "sign in first" }, 401);
  await next();
};

const app = new Hono<App>()
  .use(async (c, next) => {
    if (!c.env.DASHBOARD_TOKEN || !c.env.DASHBOARD_PASSWORD) {
      return c.json({ error: "dashboard secrets are not set" }, 500);
    }
    await next();
    c.header("cache-control", "no-store");
  })

  .post("/auth/login", rateLimited, body(login), async (c) => {
    const { password, code } = c.req.valid("json");
    if (!(await timingSafeEqual(password, c.env.DASHBOARD_PASSWORD))) {
      return c.json({ error: "wrong password" }, 401);
    }
    // The bot checks the code, or says none is set up, and tells Telegram.
    const check = await toBot(c, "/api/sign-in", "POST", JSON.stringify({ code }));
    return check.ok ? startSession(c) : check;
  })

  /** The message a linked wallet signs to sign in. The bot names this host in it. */
  .post("/auth/wallet/challenge", rateLimited, body(challenge), (c) => {
    const { address } = c.req.valid("json");
    return toBot(c, "/api/wallet/challenge", "POST", JSON.stringify({ address, purpose: "sign-in" }));
  })

  /** In place of the password: the bot checks the signature, that the wallet is linked,
   * and the code, and tells Telegram. */
  .post("/auth/wallet", rateLimited, body(walletLogin), async (c) => {
    const check = await toBot(c, "/api/sign-in/wallet", "POST", JSON.stringify(c.req.valid("json")));
    return check.ok ? startSession(c) : check;
  })

  .post("/auth/logout", (c) => {
    deleteCookie(c, COOKIE, { path: "/" });
    return c.json({ ok: true });
  })

  .get("/auth/session", async (c) => c.json({ signedIn: await hasSession(c) }))

  /** Only the method, path, query and body pass through, never the browser's headers. */
  .all("/api/*", signedIn, async (c) => {
    const { pathname, search } = new URL(c.req.url);
    const method = c.req.method;
    const payload = method === "GET" || method === "HEAD" ? undefined : await c.req.text();
    return toBot(c, pathname + search, method, payload);
  })

  .notFound((c) => c.json({ error: "not found" }, 404))
  .onError((err, c) => {
    if (err instanceof HTTPException) return c.json({ error: err.message }, err.status);
    console.error(err);
    return c.json({ error: "Something went wrong." }, 500);
  });

export default app satisfies ExportedHandler<Env>;

async function startSession(c: Ctx): Promise<Response> {
  const expires = Math.floor(Date.now() / 1000) + SESSION_SECONDS;
  await setSignedCookie(c, COOKIE, String(expires), secret(c), {
    path: "/",
    httpOnly: true,
    sameSite: "Strict",
    maxAge: SESSION_SECONDS,
    // Everywhere but plain-http localhost, where browsers would drop it.
    secure: new URL(c.req.url).protocol === "https:",
  });
  return c.json({ ok: true });
}

/** A validly signed cookie that hasn't expired. The signature covers the expiry. */
async function hasSession(c: Ctx): Promise<boolean> {
  const expires = await getSignedCookie(c, secret(c), COOKIE);
  return !!expires && Number(expires) > Date.now() / 1000;
}

/** Calls the bot with its token and this dashboard's host (for wallet messages). The
 * reply goes back as JSON with none of the bot's own headers. */
async function toBot(c: Ctx, path: string, method: string, payload?: string): Promise<Response> {
  try {
    const reply = await proxy(new URL(path, c.env.BOT_API_URL), {
      method,
      body: payload,
      headers: {
        authorization: `Bearer ${c.env.DASHBOARD_TOKEN}`,
        "content-type": "application/json",
        "x-dashboard-host": new URL(c.req.url).host,
      },
    });
    return new Response(reply.body, { status: reply.status, headers: NO_STORE });
  } catch {
    return c.json({ error: "Agile Money is not reachable. Is the bot running?" }, 502);
  }
}
