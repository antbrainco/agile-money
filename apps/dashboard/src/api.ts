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
  /** Authenticator codes are set up; resuming and raising limits ask for one. */
  codes: boolean;
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
  versions: { version: number; source: "config" | "dashboard" | "contacts"; created_at: string }[];
}

export interface ContactRow {
  id: string;
  label: string;
  address: string;
  /** Start and end of the address, to check by eye. */
  check: string;
  state: "proposed" | "confirmed" | "active";
  source: "config" | "telegram" | "dashboard" | "agent";
  replaces: boolean;
  proposed_at: string;
  active_at: string | null;
}

export interface ContactsView {
  contacts: ContactRow[];
  codes: boolean;
  cooling_off: string;
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
  login: (password: string, code?: string) =>
    call<{ ok: true }>("/auth/login", send({ password, code })),
  logout: () => call<{ ok: true }>("/auth/logout", send({})),
  overview: () => call<Overview>("/api/overview"),
  approvals: () => call<IntentRow[]>("/api/approvals"),
  answer: (approval: string, action: "approve" | "reject") =>
    call<{ message: string }>(`/api/approvals/${approval}/${action}`, { method: "POST" }),
  intents: () => call<IntentRow[]>("/api/intents"),
  invoices: () => call<InvoiceRow[]>("/api/invoices"),
  policy: () => call<PolicyView>("/api/policy"),
  savePolicy: (version: number, limits: Limits[], code?: string) =>
    call<PolicyView>("/api/policy", send({ version, limits, code }, "PUT")),
  pause: (paused: boolean, code?: string) => call<Overview>("/api/pause", send({ paused, code })),
  contacts: () => call<ContactsView>("/api/contacts"),
  addContact: (label: string, address: string, code: string) =>
    call<ContactsView>("/api/contacts", send({ label, address, code })),
  changeContact: (id: string, action: "cancel" | "remove") =>
    call<ContactsView>(`/api/contacts/${id}/${action}`, { method: "POST" }),
};
