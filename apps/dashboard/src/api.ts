// Types mirror apps/telegram/bot/src/dashboard.rs.

export interface Money {
  /** Major units, exact. */
  amount: string;
  asset: string;
}

export interface Overview {
  wallet: string;
  cluster: string;
  paused: boolean;
  policy_version: number;
  awaiting_approval: number;
}

export interface IntentRow {
  id: string;
  created_at: string;
  updated_at: string;
  state: string;
  money: Money;
  to: string;
  address: string;
  memo: string | null;
  reasons: string[];
  approval: { id: string; expires_at: string; outcome: string | null } | null;
  failure: string | null;
  link: string | null;
}

export interface InvoiceRow {
  id: string;
  created_at: string;
  client: string;
  money: Money;
  rail: string;
  url: string;
  state: "open" | "paid" | "cancelled";
  received: Money | null;
  reason: string | null;
}

export interface Limits {
  asset: string;
  max_per_tx: string;
  approval_above: string;
  max_per_day: string;
}

export interface PolicyView {
  version: number;
  limits: Limits[];
  recipients: number;
  versions: { version: number; source: "config" | "dashboard"; created_at: string }[];
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const reply = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json" },
    credentials: "same-origin",
  });
  const body = (await reply.json().catch(() => ({}))) as { error?: string };
  if (!reply.ok) throw new ApiError(body.error ?? `HTTP ${reply.status}`, reply.status);
  return body as T;
}

const send = (body: unknown, method = "POST"): RequestInit => ({
  method,
  body: JSON.stringify(body),
});

export const api = {
  session: () => call<{ signedIn: boolean }>("/auth/session"),
  login: (password: string) => call<{ ok: true }>("/auth/login", send({ password })),
  logout: () => call<{ ok: true }>("/auth/logout", send({})),
  overview: () => call<Overview>("/api/overview"),
  approvals: () => call<IntentRow[]>("/api/approvals"),
  answer: (approval: string, action: "approve" | "reject") =>
    call<{ message: string }>(`/api/approvals/${approval}/${action}`, { method: "POST" }),
  intents: () => call<IntentRow[]>("/api/intents"),
  invoices: () => call<InvoiceRow[]>("/api/invoices"),
  policy: () => call<PolicyView>("/api/policy"),
  savePolicy: (version: number, limits: Limits[]) =>
    call<PolicyView>("/api/policy", send({ version, limits }, "PUT")),
  pause: (paused: boolean) => call<Overview>("/api/pause", send({ paused })),
};
