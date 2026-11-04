import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import {
  api,
  ApiError,
  type ContactRow,
  type IntentRow,
  type InvoiceRow,
  type Limits,
  type Overview,
  type PolicyView,
} from "./api";

type Tab = "approvals" | "payments" | "requests" | "contacts" | "limits";

const TABS: { id: Tab; label: string }[] = [
  { id: "approvals", label: "Approvals" },
  { id: "payments", label: "Payments" },
  { id: "requests", label: "Requests" },
  { id: "contacts", label: "Contacts" },
  { id: "limits", label: "Limits" },
];

/** Approvals expire in 15 minutes, so poll often enough to matter. */
const POLL_MS = 5000;

export function App() {
  const [signedIn, setSignedIn] = useState<boolean | null>(null);
  useEffect(() => {
    api
      .session()
      .then((s) => setSignedIn(s.signedIn))
      .catch(() => setSignedIn(false));
  }, []);
  if (signedIn === null) return <main className="center muted">Loading…</main>;
  if (!signedIn) return <Login onDone={() => setSignedIn(true)} />;
  return <Dashboard onSignOut={() => setSignedIn(false)} />;
}

function Login({ onDone }: { onDone: () => void }) {
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.login(password);
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <main className="center">
      <form className="card login" onSubmit={submit}>
        <h1>Tilly</h1>
        <p className="muted">Approvals, payments and limits.</p>
        <label>
          Password
          <input
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoFocus
          />
        </label>
        {error && <p className="error">{error}</p>}
        <button className="primary" disabled={busy || !password}>
          {busy ? "Signing in…" : "Sign in"}
        </button>
      </form>
    </main>
  );
}

/** Loads `load` now and every `every` ms; `reload` refetches at once. */
function usePoll<T>(load: () => Promise<T>, every?: number) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(async () => {
    try {
      setData(await load());
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [load]);
  useEffect(() => {
    void reload();
    if (!every) return;
    const id = setInterval(() => void reload(), every);
    return () => clearInterval(id);
  }, [reload, every]);
  return { data, error, reload, setData };
}

function Dashboard({ onSignOut }: { onSignOut: () => void }) {
  const [tab, setTab] = useState<Tab>("approvals");
  const overview = usePoll(api.overview, POLL_MS);
  const signedOut = overview.error && /sign in/i.test(overview.error);
  useEffect(() => {
    if (signedOut) onSignOut();
  }, [signedOut, onSignOut]);

  const signOut = async () => {
    await api.logout().catch(() => undefined);
    onSignOut();
  };

  return (
    <div className="shell">
      <header>
        <div className="brand">
          <strong>Tilly</strong>
          {overview.data && (
            <span className="muted mono" title={overview.data.wallet}>
              {short(overview.data.wallet)} · {overview.data.cluster}
            </span>
          )}
        </div>
        <div className="header-actions">
          {overview.data && (
            <PauseSwitch overview={overview.data} onChange={overview.setData} />
          )}
          <button className="ghost" onClick={signOut}>
            Sign out
          </button>
        </div>
      </header>
      {overview.error && !signedOut && <p className="banner error">{overview.error}</p>}
      <nav>
        {TABS.map((t) => (
          <button
            key={t.id}
            className={tab === t.id ? "tab active" : "tab"}
            onClick={() => setTab(t.id)}
          >
            {t.label}
            {t.id === "approvals" && !!overview.data?.awaiting_approval && (
              <span className="count">{overview.data.awaiting_approval}</span>
            )}
          </button>
        ))}
      </nav>
      <main>
        {tab === "approvals" && <Approvals onAnswered={overview.reload} />}
        {tab === "payments" && <Payments />}
        {tab === "requests" && <Requests />}
        {tab === "contacts" && <Contacts />}
        {tab === "limits" && <LimitsPage codes={!!overview.data?.codes} />}
      </main>
    </div>
  );
}

function PauseSwitch({
  overview,
  onChange,
}: {
  overview: Overview;
  onChange: (o: Overview) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState(false);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const flip = async (pausing: boolean, given?: string) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.pause(pausing, given));
      setAsking(false);
      setCode("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  const click = () => {
    if (!overview.paused) {
      if (confirm("Pause all payments? Nothing is sent until you resume.")) void flip(true);
    } else if (overview.codes) {
      setAsking(true);
    } else {
      void flip(false);
    }
  };
  if (asking) {
    // Pausing is free; resuming loosens, so it takes a code.
    return (
      <form
        className="code-inline"
        onSubmit={(e) => {
          e.preventDefault();
          void flip(false, code);
        }}
      >
        <CodeInput value={code} onChange={setCode} />
        <button className="primary" disabled={busy || code.length < 6}>
          Resume
        </button>
        <button type="button" className="ghost" onClick={() => setAsking(false)}>
          Cancel
        </button>
        {error && <span className="error small">{error}</span>}
      </form>
    );
  }
  return (
    <>
      <button className={overview.paused ? "danger" : "ghost"} onClick={click} disabled={busy}>
        {overview.paused ? "Paused · Resume" : "Pause payments"}
      </button>
      {error && <span className="error small">{error}</span>}
    </>
  );
}

function Approvals({ onAnswered }: { onAnswered: () => void }) {
  const { data, error, reload } = usePoll(api.approvals, POLL_MS);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const answer = async (row: IntentRow, action: "approve" | "reject") => {
    if (!row.approval) return;
    setBusy(row.approval.id);
    try {
      const { message } = await api.answer(row.approval.id, action);
      setNotice(`${row.money.amount} ${row.money.asset} to ${row.to}: ${message}`);
    } catch (err) {
      // Usually "Already answered." from Telegram first: rule 8, first answer wins.
      setNotice(err instanceof ApiError ? err.message : String(err));
    } finally {
      setBusy(null);
      void reload();
      onAnswered();
    }
  };

  return (
    <section>
      <h2>Waiting for you</h2>
      <p className="muted">
        Answer here or in Telegram. Whichever comes first counts; the other closes.
      </p>
      {notice && <p className="banner">{notice}</p>}
      <Status error={error} loading={!data} empty={data?.length === 0} emptyText="Nothing to approve." />
      <div className="cards">
        {data?.map((row) => (
          <article key={row.id} className="card approval">
            <div className="amount">
              {row.money.amount} <span>{row.money.asset}</span>
            </div>
            <div>
              to <strong>{row.to}</strong>{" "}
              <span className="muted mono" title={row.address}>
                {short(row.address)}
              </span>
            </div>
            {row.memo && <div className="memo">“{row.memo}”</div>}
            {row.reasons.length > 0 && (
              <ul className="reasons">
                {row.reasons.map((r) => (
                  <li key={r}>{r}</li>
                ))}
              </ul>
            )}
            {row.approval && <Expiry at={row.approval.expires_at} />}
            <div className="actions">
              <button
                className="primary"
                disabled={busy === row.approval?.id}
                onClick={() => answer(row, "approve")}
              >
                Approve
              </button>
              <button
                className="ghost"
                disabled={busy === row.approval?.id}
                onClick={() => answer(row, "reject")}
              >
                Reject
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}

function Expiry({ at }: { at: string }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  const left = Math.max(0, new Date(at).getTime() - now);
  const minutes = Math.floor(left / 60000);
  const seconds = Math.floor((left % 60000) / 1000);
  return (
    <div className={left < 60000 ? "expiry soon" : "expiry"}>
      {left === 0 ? "Expired" : `Expires in ${minutes}:${String(seconds).padStart(2, "0")}`}
    </div>
  );
}

const STATE_LABEL: Record<string, string> = {
  proposed: "Proposed",
  awaiting_approval: "Waiting",
  approved: "Approved",
  submitted: "Sending",
  failed: "Retrying",
  confirmed: "Paid",
  denied: "Denied",
  rejected: "Rejected",
  abandoned: "Failed",
};

function Payments() {
  const { data, error } = usePoll(api.intents, POLL_MS * 2);
  return (
    <section>
      <h2>Payments</h2>
      <p className="muted">Everything the agent proposed, newest first.</p>
      <Status error={error} loading={!data} empty={data?.length === 0} emptyText="No payments yet." />
      {data && data.length > 0 && (
        <Table head={["When", "To", "Amount", "Status", ""]}>
          {data.map((row) => (
            <tr key={row.id}>
              <td className="muted">{when(row.created_at)}</td>
              <td>
                {row.to}
                {row.memo && <div className="muted small">{row.memo}</div>}
              </td>
              <td className="num">
                {row.money.amount} {row.money.asset}
              </td>
              <td>
                <span className={`pill ${row.state}`}>{STATE_LABEL[row.state] ?? row.state}</span>
                {why(row) && <div className="muted small">{why(row)}</div>}
              </td>
              <td>
                {row.link && (
                  <a href={row.link} target="_blank" rel="noreferrer">
                    Explorer
                  </a>
                )}
              </td>
            </tr>
          ))}
        </Table>
      )}
    </section>
  );
}

/** One line on why a payment ended where it did. */
function why(row: IntentRow): string | null {
  if (row.state === "denied") return row.reasons.join("; ");
  if (row.approval?.outcome === "expired") return "Approval expired";
  if (row.approval?.outcome === "rejected") return "You rejected it";
  if (row.approval?.outcome === "granted") return "You approved it";
  if (row.failure && row.state !== "confirmed") return row.failure;
  return null;
}

function Requests() {
  const { data, error } = usePoll(api.invoices, POLL_MS * 2);
  return (
    <section>
      <h2>Payment requests</h2>
      <p className="muted">Links Tilly made for clients to pay you.</p>
      <Status error={error} loading={!data} empty={data?.length === 0} emptyText="No requests yet." />
      {data && data.length > 0 && (
        <Table head={["When", "From", "Amount", "Via", "Status"]}>
          {data.map((row: InvoiceRow) => (
            <tr key={row.id}>
              <td className="muted">{when(row.created_at)}</td>
              <td>{row.client}</td>
              <td className="num">
                {row.money.amount} {row.money.asset}
              </td>
              <td>{row.rail === "paypal" ? "PayPal" : "Solana Pay"}</td>
              <td>
                <span className={`pill ${row.state}`}>
                  {row.state === "open" ? "Waiting" : row.state === "paid" ? "Paid" : "Cancelled"}
                </span>
                {row.state === "open" && row.url.startsWith("https://") && (
                  <a className="small" href={row.url} target="_blank" rel="noreferrer">
                    {" "}
                    Link
                  </a>
                )}
                {row.reason && <div className="muted small">{row.reason}</div>}
              </td>
            </tr>
          ))}
        </Table>
      )}
    </section>
  );
}

function LimitsPage({ codes }: { codes: boolean }) {
  const { data, error, setData } = usePoll(api.policy);
  const [draft, setDraft] = useState<Limits[] | null>(null);
  const [code, setCode] = useState("");
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    if (data) setDraft(data.limits.map((l) => ({ ...l })));
  }, [data]);

  const edit = (asset: string, field: keyof Limits, value: string) =>
    setDraft((d) => d?.map((l) => (l.asset === asset ? { ...l, [field]: value } : l)) ?? null);

  const changed =
    !!data && !!draft && JSON.stringify(draft) !== JSON.stringify(data.limits);

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!data || !draft) return;
    setSaving(true);
    setSaveError(null);
    try {
      const next: PolicyView = await api.savePolicy(data.version, draft, code || undefined);
      setData(next);
      setCode("");
      setNotice(`Saved as version ${next.version}. It applies to the next payment.`);
    } catch (err) {
      setSaveError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section>
      <h2>Limits</h2>
      <p className="muted">
        Only you change these; the agent can't. Each save is a new version. Lowering a limit is
        always free{codes ? "; raising one needs a code from your authenticator app" : ""}.
      </p>
      <Status error={error} loading={!data} />
      {notice && <p className="banner">{notice}</p>}
      {draft && (
        <form onSubmit={save}>
          <Table head={["Asset", "Max per payment", "Ask me above", "Max per 24 h"]}>
            {draft.map((l) => (
              <tr key={l.asset}>
                <td>
                  <strong>{l.asset}</strong>
                </td>
                {(["max_per_tx", "approval_above", "max_per_day"] as const).map((field) => (
                  <td key={field}>
                    <input
                      className="num"
                      inputMode="decimal"
                      value={l[field]}
                      onChange={(e) => edit(l.asset, field, e.target.value)}
                      aria-label={`${l.asset} ${field}`}
                    />
                  </td>
                ))}
              </tr>
            ))}
          </Table>
          {saveError && <p className="error">{saveError}</p>}
          <div className="actions">
            {codes && changed && <CodeInput value={code} onChange={setCode} optional />}
            <button className="primary" disabled={!changed || saving}>
              {saving ? "Saving…" : `Save as version ${(data?.version ?? 0) + 1}`}
            </button>
            {changed && (
              <button type="button" className="ghost" onClick={() => setData({ ...data! })}>
                Discard
              </button>
            )}
          </div>
        </form>
      )}
      {data && (
        <>
          <h3>History</h3>
          <Table head={["Version", "Saved", "From"]}>
            {data.versions.map((v) => (
              <tr key={v.version}>
                <td>
                  v{v.version}
                  {v.version === data.version && <span className="pill confirmed">current</span>}
                </td>
                <td className="muted">{when(v.created_at)}</td>
                <td>{SOURCE_LABEL[v.source]}</td>
              </tr>
            ))}
          </Table>
        </>
      )}
    </section>
  );
}

const SOURCE_LABEL = {
  config: "warden.toml",
  dashboard: "Dashboard",
  contacts: "Contacts changed",
} as const;

const CONTACT_FROM: Record<ContactRow["source"], string> = {
  config: "warden.toml",
  telegram: "Telegram",
  dashboard: "Dashboard",
  agent: "Suggested by Tilly",
};

function Contacts() {
  const { data, error, reload, setData } = usePoll(api.contacts, POLL_MS * 2);
  const [label, setLabel] = useState("");
  const [address, setAddress] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setFormError(null);
    try {
      setData(await api.addContact(label.trim(), address.trim(), code));
      setNotice(
        `${label.trim()} is saved and can be paid in ${data?.cooling_off ?? "a while"}. ` +
          "Telegram has a Cancel button until then.",
      );
      setLabel("");
      setAddress("");
      setCode("");
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const change = async (row: ContactRow, action: "cancel" | "remove") => {
    const question =
      action === "remove"
        ? `Remove ${row.label}? Tilly won't be able to pay them until you add them again.`
        : `Cancel ${row.label}?`;
    if (!confirm(question)) return;
    try {
      setData(await api.changeContact(row.id, action));
    } catch (err) {
      setNotice(err instanceof Error ? err.message : String(err));
      void reload();
    }
  };

  return (
    <section>
      <h2>Contacts</h2>
      <p className="muted">
        The only people Tilly can pay. A new or changed address waits{" "}
        {data?.cooling_off ?? "a while"} before it can be paid, and the first payment to anyone
        always asks you. Removing is instant and needs no code.
      </p>
      {notice && <p className="banner">{notice}</p>}
      <Status error={error} loading={!data} empty={data?.contacts.length === 0} emptyText="No contacts yet." />
      {data && data.contacts.length > 0 && (
        <Table head={["Name", "Address", "Status", ""]}>
          {data.contacts.map((row) => (
            <tr key={row.id}>
              <td>
                <strong>{row.label}</strong>
                <div className="muted small">{CONTACT_FROM[row.source]}</div>
              </td>
              <td className="mono" title={row.address}>
                {row.check}
              </td>
              <td>
                <ContactState row={row} />
              </td>
              <td>
                <button
                  className="ghost small"
                  onClick={() => change(row, row.state === "active" ? "remove" : "cancel")}
                >
                  {row.state === "active" ? "Remove" : "Cancel"}
                </button>
              </td>
            </tr>
          ))}
        </Table>
      )}
      <h3>Add or change a contact</h3>
      {data && !data.codes ? (
        <p className="muted">
          Set up authenticator codes on the server first (<code>--example totp_setup</code>), then
          restart Tilly. Until then contacts come from warden.toml only.
        </p>
      ) : (
        <form className="card contact-form" onSubmit={add}>
          <label>
            Name
            <input value={label} maxLength={40} onChange={(e) => setLabel(e.target.value)} />
          </label>
          <label>
            Address
            <input
              className="mono"
              value={address}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setAddress(e.target.value)}
            />
          </label>
          {address.trim().length > 14 && (
            <p className="muted small">
              Check it starts and ends as you expect:{" "}
              <strong className="mono">{short(address.trim())}</strong>
            </p>
          )}
          <p className="muted small">
            Using a saved name replaces that contact's address once the wait is over.
          </p>
          <div className="actions">
            <CodeInput value={code} onChange={setCode} />
            <button className="primary" disabled={busy || !label.trim() || !address.trim() || code.length < 6}>
              {busy ? "Saving…" : "Save contact"}
            </button>
          </div>
          {formError && <p className="error">{formError}</p>}
        </form>
      )}
    </section>
  );
}

function ContactState({ row }: { row: ContactRow }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (row.state !== "confirmed") return;
    const id = setInterval(() => setNow(Date.now()), 30000);
    return () => clearInterval(id);
  }, [row.state]);
  if (row.state === "active") return <span className="pill confirmed">Can be paid</span>;
  if (row.state === "proposed") return <span className="pill open">Needs your code</span>;
  const left = row.active_at ? Math.max(0, new Date(row.active_at).getTime() - now) : 0;
  const hours = Math.floor(left / 3600000);
  const minutes = Math.ceil((left % 3600000) / 60000);
  return (
    <>
      <span className="pill open">{row.replaces ? "Address changing" : "Waiting"}</span>
      <div className="muted small">
        {left === 0 ? "Any moment now" : `Live in ${hours ? `${hours} h ` : ""}${minutes} min`}
      </div>
    </>
  );
}

/** Six digits from the owner's authenticator app. */
function CodeInput({
  value,
  onChange,
  optional,
}: {
  value: string;
  onChange: (code: string) => void;
  optional?: boolean;
}) {
  return (
    <input
      className="code mono"
      inputMode="numeric"
      autoComplete="one-time-code"
      maxLength={6}
      placeholder={optional ? "Code if raising" : "6-digit code"}
      aria-label="Authenticator code"
      value={value}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, "").slice(0, 6))}
    />
  );
}

function Status({
  error,
  loading,
  empty,
  emptyText,
}: {
  error: string | null;
  loading: boolean;
  empty?: boolean;
  emptyText?: string;
}) {
  if (error) return <p className="error">{error}</p>;
  if (loading) return <p className="muted">Loading…</p>;
  if (empty) return <p className="muted empty">{emptyText}</p>;
  return null;
}

function Table({ head, children }: { head: string[]; children: ReactNode }) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            {head.map((h, i) => (
              <th key={i}>{h}</th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

function short(address: string): string {
  return address.length > 14 ? `${address.slice(0, 4)}…${address.slice(-4)}` : address;
}

function when(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
