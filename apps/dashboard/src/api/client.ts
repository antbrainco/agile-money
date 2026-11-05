// Plain calls to the Worker. No caching here: server state lives in ./queries.ts.

import type {
  CodeSetup,
  ContactsView,
  IntentRow,
  InvoiceRow,
  Limits,
  Overview,
  PolicyView,
  Profile,
} from "./types";

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
  walletChallenge: (address: string) =>
    call<{ message: string }>("/auth/wallet/challenge", send({ address })),
  walletLogin: (address: string, signature: string, code?: string) =>
    call<{ ok: true }>("/auth/wallet", send({ address, signature, code })),
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
  codesSetup: () => call<CodeSetup>("/api/codes/setup", send({})),
  profile: () => call<Profile>("/api/profile"),
  linkChallenge: (address: string) =>
    call<{ message: string }>("/api/wallet/challenge", send({ address, purpose: "link" })),
  linkWallet: (address: string, signature: string, label: string, code?: string) =>
    call<Profile>("/api/wallets", send({ address, signature, label, code })),
  unlinkWallet: (address: string) =>
    call<Profile>(`/api/wallets/${encodeURIComponent(address)}/remove`, { method: "POST" }),
  telegramLink: () => call<{ url: string }>("/api/telegram/link", send({})),
  codesConfirm: (code: string) => call<Overview>("/api/codes/confirm", send({ code })),
};
