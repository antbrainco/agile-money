// The dashboard's Worker: the owner signs in with a password and, once codes are
// set up, an authenticator code the bot checks. Then `/api/*` is forwarded to the
// bot's API with the bot's token. The token never reaches the browser; the
// browser holds only a signed, short-lived session cookie.

interface Secrets {
  /** The bot's DASHBOARD_TOKEN. */
  DASHBOARD_TOKEN: string;
  /** The owner's sign-in. */
  DASHBOARD_PASSWORD: string;
}

type Bindings = Env & Secrets;

const COOKIE = "tilly_session";
/** Sessions last 12 hours. */
const SESSION_SECONDS = 12 * 60 * 60;

export default {
  async fetch(request: Request, env: Bindings): Promise<Response> {
    const url = new URL(request.url);
    if (!env.DASHBOARD_TOKEN || !env.DASHBOARD_PASSWORD) {
      return json({ error: "dashboard secrets are not set" }, 500);
    }
    if (url.pathname === "/auth/login" && request.method === "POST") {
      return login(request, env);
    }
    if (url.pathname === "/auth/logout" && request.method === "POST") {
      return json({ ok: true }, 200, cookie("", 0, url));
    }
    if (url.pathname === "/auth/session") {
      return json({ signedIn: await signedIn(request, env) });
    }
    if (url.pathname.startsWith("/api/")) {
      if (!(await signedIn(request, env))) {
        return json({ error: "sign in first" }, 401);
      }
      return forward(request, url, env);
    }
    return json({ error: "not found" }, 404);
  },
} satisfies ExportedHandler<Bindings>;

async function login(request: Request, env: Bindings): Promise<Response> {
  // Slows password guessing. Approximate and per location; the bot's code check
  // behind it is strict (five wrong codes lock codes for 15 minutes).
  const ip = request.headers.get("cf-connecting-ip") ?? "local";
  const [perIp, overall] = await Promise.all([
    env.LOGIN_LIMIT.limit({ key: `ip:${ip}` }),
    env.LOGIN_LIMIT_ALL.limit({ key: "all" }),
  ]);
  if (!perIp.success || !overall.success) {
    return json({ error: "Too many sign-in attempts. Wait a minute." }, 429);
  }
  const body = (await request.json().catch(() => ({}))) as { password?: unknown; code?: unknown };
  // Copying from a file often brings a trailing newline or space along.
  const given = typeof body.password === "string" ? body.password.trim() : "";
  if (!(await equal(given, env.DASHBOARD_PASSWORD))) {
    return json({ error: "wrong password" }, 401);
  }
  // The bot checks the code, or says none is set up, and tells Telegram.
  const code = typeof body.code === "string" ? body.code : undefined;
  const check = await forwardJson("/api/sign-in", { code }, env);
  if (!check.ok) return check;
  const expires = Math.floor(Date.now() / 1000) + SESSION_SECONDS;
  const value = `${expires}.${await sign(String(expires), env)}`;
  return json({ ok: true }, 200, cookie(value, SESSION_SECONDS, new URL(request.url)));
}

async function signedIn(request: Request, env: Bindings): Promise<boolean> {
  const value = readCookie(request, COOKIE);
  if (!value) return false;
  const [expires, mac] = value.split(".");
  if (!expires || !mac || Number(expires) < Date.now() / 1000) return false;
  return equal(mac, await sign(expires, env));
}

/** Forwards to the bot. Only the method, path, query and JSON body pass through. */
async function forward(request: Request, url: URL, env: Bindings): Promise<Response> {
  const target = new URL(url.pathname + url.search, env.BOT_API_URL);
  const init: RequestInit = {
    method: request.method,
    headers: {
      authorization: `Bearer ${env.DASHBOARD_TOKEN}`,
      "content-type": "application/json",
    },
  };
  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = await request.text();
  }
  try {
    const reply = await fetch(target, init);
    return new Response(reply.body, {
      status: reply.status,
      headers: { "content-type": "application/json", "cache-control": "no-store" },
    });
  } catch {
    return json({ error: "Tilly is not reachable. Is the bot running?" }, 502);
  }
}

/** POSTs JSON to the bot from the Worker itself, not on the browser's behalf. */
async function forwardJson(path: string, body: unknown, env: Bindings): Promise<Response> {
  try {
    const reply = await fetch(new URL(path, env.BOT_API_URL), {
      method: "POST",
      headers: {
        authorization: `Bearer ${env.DASHBOARD_TOKEN}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return new Response(reply.body, {
      status: reply.status,
      headers: { "content-type": "application/json", "cache-control": "no-store" },
    });
  } catch {
    return json({ error: "Tilly is not reachable. Is the bot running?" }, 502);
  }
}

/** HMAC-SHA256 under the bot token, so rotating the token signs everyone out. */
async function sign(text: string, env: Bindings): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(`session:${env.DASHBOARD_TOKEN}`),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(text));
  return btoa(String.fromCharCode(...new Uint8Array(mac)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

/** Compares digests, so the time taken says nothing about where the inputs differ. */
async function equal(a: string, b: string): Promise<boolean> {
  const digest = async (s: string) =>
    new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s)));
  const [x, y] = await Promise.all([digest(a), digest(b)]);
  let diff = 0;
  for (let i = 0; i < x.length; i++) diff |= x[i]! ^ y[i]!;
  return diff === 0;
}

function readCookie(request: Request, name: string): string | undefined {
  const header = request.headers.get("cookie") ?? "";
  for (const part of header.split(";")) {
    const [key, ...rest] = part.trim().split("=");
    if (key === name) return rest.join("=");
  }
  return undefined;
}

function cookie(value: string, maxAge: number, url: URL): HeadersInit {
  // Secure everywhere but plain-http localhost, where browsers would drop it.
  const secure = url.protocol === "https:" ? "; Secure" : "";
  return {
    "set-cookie": `${COOKIE}=${value}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${maxAge}${secure}`,
  };
}

function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}
