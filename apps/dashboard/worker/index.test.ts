import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import worker from "./index";

const BOT = "https://bot.test";
const allow = { limit: async () => ({ success: true }) };
const deny = { limit: async () => ({ success: false }) };

function env(overrides: Record<string, unknown> = {}) {
  return {
    BOT_API_URL: BOT,
    DASHBOARD_TOKEN: "bot-token",
    DASHBOARD_PASSWORD: "correct horse",
    LOGIN_LIMIT: allow,
    LOGIN_LIMIT_ALL: allow,
    ...overrides,
  } as unknown as Parameters<typeof worker.fetch>[1];
}

function post(path: string, body: unknown, headers: Record<string, string> = {}) {
  return new Request(`https://dash.test${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify(body),
  });
}

/** The session cookie a response sets, as `name=value`. */
function sessionCookie(res: Response): string {
  return (res.headers.get("set-cookie") ?? "").split(";")[0]!;
}

let bot: ReturnType<typeof vi.fn>;

/** The request the Worker sent the bot, by call. */
const sent = (i = 0) => new Request(bot.mock.calls[i]![0], bot.mock.calls[i]![1]);

beforeEach(() => {
  bot = vi.fn(async () => Response.json({ ok: true }));
  vi.stubGlobal("fetch", bot);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("sign-in", () => {
  it("refuses to run without its secrets", async () => {
    const res = await worker.fetch(post("/auth/login", {}), env({ DASHBOARD_PASSWORD: "" }));
    expect(res.status).toBe(500);
  });

  it("rejects a wrong password without asking the bot", async () => {
    const res = await worker.fetch(post("/auth/login", { password: "nope" }), env());
    expect(res.status).toBe(401);
    expect(bot).not.toHaveBeenCalled();
  });

  it("checks the code with the bot, then sets a secure session cookie", async () => {
    const res = await worker.fetch(
      post("/auth/login", { password: "correct horse\n", code: "123456" }),
      env(),
    );
    expect(res.status).toBe(200);
    const req = sent();
    expect(req.url).toBe(`${BOT}/api/sign-in`);
    expect(await req.json()).toEqual({ code: "123456" });
    expect(req.headers.get("authorization")).toBe("Bearer bot-token");
    expect(req.headers.get("x-dashboard-host")).toBe("dash.test");
    const cookie = res.headers.get("set-cookie")!;
    expect(cookie).toMatch(/HttpOnly/);
    expect(cookie).toMatch(/SameSite=Strict/);
    expect(cookie).toMatch(/Secure/);
  });

  it("passes on the bot's refusal of a wrong code", async () => {
    bot.mockResolvedValueOnce(Response.json({ error: "wrong code" }, { status: 401 }));
    const res = await worker.fetch(post("/auth/login", { password: "correct horse" }), env());
    expect(res.status).toBe(401);
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("rejects a malformed body without asking the bot", async () => {
    for (const body of [{ password: 42 }, {}]) {
      const res = await worker.fetch(post("/auth/login", body), env());
      expect(res.status).toBe(400);
    }
    const garbled = new Request("https://dash.test/auth/login", { method: "POST", body: "{" });
    expect((await worker.fetch(garbled, env())).status).toBe(400);
    expect(bot).not.toHaveBeenCalled();
  });

  it("is rate limited", async () => {
    const res = await worker.fetch(
      post("/auth/login", { password: "correct horse" }),
      env({ LOGIN_LIMIT: deny }),
    );
    expect(res.status).toBe(429);
    expect(bot).not.toHaveBeenCalled();
  });

  it("signs in with a wallet only when the bot accepts the signature", async () => {
    const body = { address: "addr", signature: "sig", code: "123456" };
    const ok = await worker.fetch(post("/auth/wallet", body), env());
    expect(ok.status).toBe(200);
    expect(await sent().json()).toEqual(body);

    bot.mockResolvedValueOnce(Response.json({ error: "bad signature" }, { status: 401 }));
    const refused = await worker.fetch(post("/auth/wallet", body), env());
    expect(refused.status).toBe(401);
    expect(refused.headers.get("set-cookie")).toBeNull();
  });
});

describe("the API proxy", () => {
  async function signIn(): Promise<string> {
    const res = await worker.fetch(post("/auth/login", { password: "correct horse" }), env());
    bot.mockClear();
    return sessionCookie(res);
  }

  it("needs a session", async () => {
    const res = await worker.fetch(new Request("https://dash.test/api/overview"), env());
    expect(res.status).toBe(401);
    expect(bot).not.toHaveBeenCalled();
  });

  it("forwards with the bot's token, which never reaches the browser", async () => {
    const cookie = await signIn();
    const res = await worker.fetch(
      new Request("https://dash.test/api/intents?limit=5", { headers: { cookie } }),
      env(),
    );
    expect(res.status).toBe(200);
    const req = sent();
    expect(req.url).toBe(`${BOT}/api/intents?limit=5`);
    expect(req.headers.get("authorization")).toBe("Bearer bot-token");
    expect(JSON.stringify([...res.headers])).not.toContain("bot-token");
  });

  it("rejects a forged or tampered cookie", async () => {
    const [name, value] = (await signIn()).split("=");
    const [expires, mac] = value!.split(".");
    for (const forged of [`${Number(expires) + 3600}.${mac}`, `${expires}.${mac}x`, "garbage"]) {
      const res = await worker.fetch(
        new Request("https://dash.test/api/overview", { headers: { cookie: `${name}=${forged}` } }),
        env(),
      );
      expect(res.status).toBe(401);
    }
  });

  it("rejects a session signed under an old bot token", async () => {
    const cookie = await signIn();
    const res = await worker.fetch(
      new Request("https://dash.test/api/overview", { headers: { cookie } }),
      env({ DASHBOARD_TOKEN: "rotated" }),
    );
    expect(res.status).toBe(401);
  });

  it("expires sessions after 12 hours", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const cookie = await signIn();
    vi.setSystemTime(Date.now() + 12 * 60 * 60 * 1000 + 1000);
    const res = await worker.fetch(
      new Request("https://dash.test/api/overview", { headers: { cookie } }),
      env(),
    );
    expect(res.status).toBe(401);
  });

  it("passes none of the browser's headers to the bot", async () => {
    const cookie = await signIn();
    await worker.fetch(
      new Request("https://dash.test/api/overview", { headers: { cookie, "x-extra": "1" } }),
      env(),
    );
    const headers = sent().headers;
    expect(headers.get("cookie")).toBeNull();
    expect(headers.get("x-extra")).toBeNull();
  });

  it("clears the cookie on sign-out", async () => {
    const res = await worker.fetch(post("/auth/logout", {}, { cookie: await signIn() }), env());
    expect(res.headers.get("set-cookie")).toMatch(/Max-Age=0/);
  });

  it("says when the bot is unreachable", async () => {
    const cookie = await signIn();
    bot.mockRejectedValueOnce(new TypeError("connection refused"));
    const res = await worker.fetch(
      new Request("https://dash.test/api/overview", { headers: { cookie } }),
      env(),
    );
    expect(res.status).toBe(502);
  });
});
