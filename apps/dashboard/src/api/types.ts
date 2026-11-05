// What the bot's dashboard API returns. Mirrors apps/telegram/bot/src/dashboard.rs.

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

/** A started authenticator enrolment. `key` and `url` hold the secret. */
export interface CodeSetup {
  url: string;
  key: string;
  /** SVG document. */
  qr: string;
}

export interface Profile {
  telegram: {
    chat_id: number;
    bot: string | null;
    name: string | null;
    username: string | null;
    linked_at: string | null;
  };
  wallets: { address: string; label: string; linked_at: string }[];
  codes: boolean;
}
