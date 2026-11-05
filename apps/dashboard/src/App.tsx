import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import {
  api,
  ApiError,
  type CodeSetup,
  type ContactRow,
  type IntentRow,
  type InvoiceRow,
  type Limits,
  type Overview,
  type PolicyView,
  type Profile,
} from "./api";
import { Icon, type IconName } from "./icons";
import { connect, declined, onWallets, signText, wallets, type Wallet } from "./wallet";

type Tab =
  | "approvals"
  | "activity"
  | "requests"
  | "contacts"
  | "limits"
  | "security"
  | "profile";

const TABS: { id: Tab; label: string; icon: IconName }[] = [
  { id: "approvals", label: "Approvals", icon: "approvals" },
  { id: "activity", label: "Activity", icon: "activity" },
  { id: "requests", label: "Requests", icon: "requests" },
  { id: "contacts", label: "Contacts", icon: "contacts" },
  { id: "limits", label: "Limits", icon: "limits" },
  { id: "security", label: "Security", icon: "security" },
];

/** Approvals expire in 15 minutes, so poll often enough to matter. */
const POLL_MS = 5000;
const APPROVAL_WINDOW_MS = 15 * 60 * 1000;

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

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

function Login({ onDone }: { onDone: () => void }) {
  const [password, setPassword] = useState("");
  const [show, setShow] = useState(false);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.login(password, code || undefined);
      onDone();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <main className="center">
      <form className="card login" onSubmit={submit}>
        <div className="mark">
          <Icon name="security" />
        </div>
        <h1>Sign in</h1>
        <p>Approve payments and set your agent's limits.</p>
        <div className="form">
          <label className="field">
            <span>Password</span>
            <div className="password">
              <input
                type={show ? "text" : "password"}
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-invalid={!!error}
                autoFocus
              />
              <button
                type="button"
                onClick={() => setShow((s) => !s)}
                aria-label={show ? "Hide password" : "Show password"}
              >
                <Icon name={show ? "eyeOff" : "eye"} />
              </button>
            </div>
          </label>
          <label className="field">
            <span>Authenticator code</span>
            <CodeInput value={code} onChange={setCode} />
            <p className="hint">Leave empty if codes aren't set up yet.</p>
          </label>
          {error && <p className="error">{error}</p>}
          <button className="primary wide" disabled={busy || !password}>
            {busy ? "Signing in…" : "Sign in"}
          </button>
          <div className="or">or</div>
          <WalletButtons
            busy={busy}
            label="Continue with"
            onPick={async (wallet) => {
              setBusy(true);
              setError(null);
              try {
                const address = await connect(wallet);
                const { message } = await api.walletChallenge(address);
                const signature = await signText(wallet, address, message);
                await api.walletLogin(address, signature, code || undefined);
                onDone();
              } catch (err) {
                if (!declined(err)) setError(message(err));
              } finally {
                setBusy(false);
              }
            }}
          />
        </div>
      </form>
    </main>
  );
}

function useWallets(): Wallet[] {
  const [list, setList] = useState<Wallet[]>(wallets);
  useEffect(() => onWallets(() => setList(wallets())), []);
  return list;
}

/** One button per wallet the browser has; a hint when there are none. */
function WalletButtons({
  busy,
  label,
  onPick,
}: {
  busy: boolean;
  label: string;
  onPick: (wallet: Wallet) => void;
}) {
  const list = useWallets();
  if (list.length === 0)
    return (
      <p className="hint center-text">
        No Solana wallet found in this browser. Install Phantom, Solflare or Backpack to sign in
        with one.
      </p>
    );
  return (
    <div className="wallet-buttons">
      {list.map((w) => (
        <button key={w.name} type="button" className="wide" disabled={busy} onClick={() => onPick(w)}>
          <img src={w.icon} alt="" width={20} height={20} />
          {label} {w.name}
        </button>
      ))}
    </div>
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
      setError(message(err));
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

/** The open page lives in the URL hash, so reload and back keep it. */
function useTab(): [Tab, (t: Tab) => void] {
  const read = (): Tab => {
    const id = location.hash.replace(/^#\/?/, "");
    return id === "profile" || TABS.some((t) => t.id === id) ? (id as Tab) : "approvals";
  };
  const [tab, setTab] = useState<Tab>(read);
  useEffect(() => {
    const onHash = () => setTab(read());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const go = (t: Tab) => {
    if (t !== tab) location.hash = `/${t}`;
    window.scrollTo({ top: 0 });
  };
  return [tab, go];
}

function Dashboard({ onSignOut }: { onSignOut: () => void }) {
  const [tab, go] = useTab();
  const overview = usePoll(api.overview, POLL_MS);
  const signedOut = overview.error && /sign in/i.test(overview.error);
  useEffect(() => {
    if (signedOut) onSignOut();
  }, [signedOut, onSignOut]);

  const signOut = async () => {
    await api.logout().catch(() => undefined);
    onSignOut();
  };
  const o = overview.data;

  return (
    <div className="shell">
      <header className="topbar">
        <nav className="tabs" aria-label="Pages">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={tab === t.id ? "tab active" : "tab"}
              onClick={() => go(t.id)}
              aria-current={tab === t.id ? "page" : undefined}
            >
              <Icon name={t.icon} />
              {t.label}
              {t.id === "approvals" && !!o?.awaiting_approval && (
                <span className="count">{o.awaiting_approval}</span>
              )}
            </button>
          ))}
        </nav>
        <span className="spacer" />
        {o && (
          <span className="wallet-chip" title={o.wallet}>
            {short(o.wallet)}
            <span className="net">{o.cluster}</span>
          </span>
        )}
        <button
          className={tab === "profile" ? "icon-btn active" : "icon-btn"}
          onClick={() => go("profile")}
          title="Profile"
          aria-label="Profile"
        >
          <Icon name="profile" />
        </button>
      </header>
      {overview.error && !signedOut && <Banner error>{overview.error}</Banner>}
      {o && <StatusStrip overview={o} onChange={overview.setData} />}
      {o && !o.codes && tab !== "security" && (
        <Banner
          icon="info"
          action={
            <button className="small primary" onClick={() => go("security")}>
              Set up
            </button>
          }
        >
          Authenticator codes are off, so the password alone can raise limits and add contacts.
        </Banner>
      )}
      <main>
        {tab === "approvals" && <Approvals onAnswered={overview.reload} />}
        {tab === "activity" && <Activity />}
        {tab === "requests" && <Requests />}
        {tab === "contacts" && <Contacts onSetUp={() => go("security")} />}
        {tab === "limits" && <LimitsPage codes={!!o?.codes} />}
        {tab === "security" && o && <Security overview={o} onChange={overview.setData} />}
        {tab === "profile" && <ProfilePage onSignOut={signOut} onSetUp={() => go("security")} />}
      </main>
    </div>
  );
}

function Banner({
  children,
  error,
  icon,
  action,
}: {
  children: ReactNode;
  error?: boolean;
  icon?: IconName;
  action?: ReactNode;
}) {
  return (
    <div className={error ? "banner error" : "banner"} role={error ? "alert" : "status"}>
      <Icon name={icon ?? (error ? "info" : "check")} />
      <span>{children}</span>
      {action}
    </div>
  );
}

/** Live or paused, and the switch. Pausing is free; resuming loosens, so it takes a code. */
function StatusStrip({
  overview,
  onChange,
}: {
  overview: Overview;
  onChange: (o: Overview) => void;
}) {
  const [sheet, setSheet] = useState<"pause" | "resume" | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const flip = async (pausing: boolean, given?: string) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.pause(pausing, given));
      setSheet(null);
      setCode("");
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  const close = () => {
    setSheet(null);
    setError(null);
    setCode("");
  };
  return (
    <>
      <section className={overview.paused ? "status paused" : "status"}>
        <div className="status-text">
          <span className={overview.paused ? "dot off" : "dot"} />
          {overview.paused ? "Payments paused" : "Live"}
          <span className="muted">
            {overview.paused
              ? "· nothing is sent until you resume"
              : `· paying within policy v${overview.policy_version}`}
          </span>
        </div>
        <button
          className={overview.paused ? "small primary" : "small ghost"}
          onClick={() =>
            overview.paused && !overview.codes ? void flip(false) : setSheet(overview.paused ? "resume" : "pause")
          }
          disabled={busy}
        >
          <Icon name={overview.paused ? "play" : "pause"} />
          {overview.paused ? "Resume" : "Pause"}
        </button>
      </section>
      {sheet === "pause" && (
        <Sheet title="Pause payments" onClose={close}>
          <div className="form">
            <p className="muted">
              Nothing is sent until you resume, here or with /resume in Telegram. Approvals already
              waiting stay open.
            </p>
            {error && <p className="error">{error}</p>}
            <button className="danger wide" disabled={busy} onClick={() => void flip(true)}>
              <Icon name="pause" />
              Pause all payments
            </button>
          </div>
        </Sheet>
      )}
      {sheet === "resume" && (
        <Sheet title="Resume payments" onClose={close}>
          <form
            className="form"
            onSubmit={(e) => {
              e.preventDefault();
              void flip(false, code);
            }}
          >
            <label className="field">
              <span>Authenticator code</span>
              <CodeInput value={code} onChange={setCode} autoFocus invalid={!!error} />
            </label>
            {error && <p className="error">{error}</p>}
            <button className="primary wide" disabled={busy || code.length < 6}>
              Resume
            </button>
          </form>
        </Sheet>
      )}
    </>
  );
}

function Sheet({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = overflow;
    };
  }, [onClose]);
  return (
    <div className="overlay" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="sheet" role="dialog" aria-modal="true" aria-label={title}>
        <div className="sheet-head">
          <h2>{title}</h2>
          <button className="icon-btn" onClick={onClose} aria-label="Close">
            <Icon name="close" />
          </button>
        </div>
        {children}
      </div>
    </div>
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
      <Head title="Approvals">
        Answer here or in Telegram. Whichever comes first counts; the other closes.
      </Head>
      {notice && <Banner>{notice}</Banner>}
      <Status
        error={error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="Nothing waiting for you."
        emptyIcon="approvals"
      />
      <div className="cards">
        {data?.map((row) => (
          <article key={row.id} className="card approval">
            <div className="approval-to">
              <Avatar name={row.to} />
              <div className="row-main">
                <div className="row-title">{row.to}</div>
                <div className="row-sub mono" title={row.address}>
                  {short(row.address)}
                </div>
              </div>
            </div>
            <div className="amount">
              {row.money.amount}
              <span className="unit">{row.money.asset}</span>
            </div>
            {row.memo && <div className="memo">“{row.memo}”</div>}
            {row.reasons.length > 0 && (
              <ul className="reasons">
                {row.reasons.map((r) => (
                  <li key={r}>
                    <Icon name="info" />
                    {r}
                  </li>
                ))}
              </ul>
            )}
            {row.approval && <Expiry at={row.approval.expires_at} />}
            <div className="actions split">
              <button
                className="ghost"
                disabled={busy === row.approval?.id}
                onClick={() => answer(row, "reject")}
              >
                Reject
              </button>
              <button
                className="primary"
                disabled={busy === row.approval?.id}
                onClick={() => answer(row, "approve")}
              >
                <Icon name="check" />
                Approve
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
      <div className="bar">
        <span style={{ width: `${Math.min(100, (left / APPROVAL_WINDOW_MS) * 100)}%` }} />
      </div>
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

const ENDED_BADLY = ["denied", "rejected", "abandoned"];

function StatePill({ state }: { state: string }) {
  const icon: IconName | null =
    state === "confirmed" ? "check" : ENDED_BADLY.includes(state) ? "close" : "clock";
  return (
    <span className={`pill ${state}`}>
      {icon && <Icon name={icon} />}
      {STATE_LABEL[state] ?? state}
    </span>
  );
}

function Activity() {
  const { data, error } = usePoll(api.intents, POLL_MS * 2);
  const [open, setOpen] = useState<IntentRow | null>(null);
  return (
    <section>
      <Head title="Activity">Everything the agent proposed, newest first.</Head>
      <Status
        error={error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="No payments yet."
        emptyIcon="activity"
      />
      {data && data.length > 0 && (
        <div className="list">
          {byDay(data).map(([day, rows]) => (
            <div key={day}>
              <div className="day">{day}</div>
              {rows.map((row) => {
                const note = why(row);
                const bad = ENDED_BADLY.includes(row.state);
                return (
                  <button key={row.id} className="row-button" onClick={() => setOpen(row)}>
                    <Avatar name={row.to} />
                    <div className="row-main">
                      <div className="row-title">{row.to}</div>
                      <div className="row-sub">
                        {time(row.created_at)}
                        {row.memo && ` · ${row.memo}`}
                      </div>
                      {note && bad && <div className="row-note bad">{note}</div>}
                    </div>
                    <div className="row-side">
                      <span className={bad ? "row-amount void" : "row-amount"}>
                        −{row.money.amount} {row.money.asset}
                      </span>
                      <StatePill state={row.state} />
                    </div>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      )}
      {open && <Receipt row={open} onClose={() => setOpen(null)} />}
    </section>
  );
}

function Receipt({ row, onClose }: { row: IntentRow; onClose: () => void }) {
  const note = why(row);
  return (
    <Sheet title="Receipt" onClose={onClose}>
      <div className="ticket">
        <div className="ticket-top">
          <span>
            <Icon name="activity" />
            Payment
          </span>
          <StatePill state={row.state} />
        </div>
        <div className="ticket-cut" />
        <div className="ticket-hero">
          <span className="avatar large">
            <Icon name="store" />
          </span>
          <span className="who">{row.to}</span>
          <span className="amount">
            −{row.money.amount}
            <span className="unit">{row.money.asset}</span>
          </span>
        </div>
        <div className="details">
          <Detail label="Date & time">{when(row.created_at)}</Detail>
          <Detail label="To">
            <span className="mono" title={row.address}>
              {short(row.address)}
            </span>
          </Detail>
          {row.memo && <Detail label="Memo">{row.memo}</Detail>}
          {note && <Detail label="Note">{note}</Detail>}
          {row.link && (
            <Detail label="Transaction">
              <a href={row.link} target="_blank" rel="noreferrer">
                View in explorer
              </a>
            </Detail>
          )}
        </div>
      </div>
    </Sheet>
  );
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="detail">
      <span>{label}</span>
      <span>{children}</span>
    </div>
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
      <Head title="Requests">Links Agile Money made for clients to pay you.</Head>
      <Status
        error={error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="No requests yet."
        emptyIcon="requests"
      />
      {data && data.length > 0 && (
        <div className="list">
          {data.map((row: InvoiceRow) => (
            <div key={row.id} className="row">
              <Avatar name={row.client} />
              <div className="row-main">
                <div className="row-title">{row.client}</div>
                <div className="row-sub">
                  {when(row.created_at)} · {row.rail === "paypal" ? "PayPal" : "Solana Pay"}
                  {row.state === "open" && row.url.startsWith("https://") && (
                    <>
                      {" · "}
                      <a href={row.url} target="_blank" rel="noreferrer">
                        Link
                      </a>
                    </>
                  )}
                </div>
                {row.reason && <div className="row-note">{row.reason}</div>}
              </div>
              <div className="row-side">
                <span className={row.state === "cancelled" ? "row-amount void" : "row-amount"}>
                  +{row.money.amount} {row.money.asset}
                </span>
                <span className={`pill ${row.state}`}>
                  <Icon
                    name={row.state === "paid" ? "check" : row.state === "open" ? "clock" : "close"}
                  />
                  {row.state === "open" ? "Waiting" : row.state === "paid" ? "Paid" : "Cancelled"}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

const LIMIT_FIELDS = [
  ["max_per_tx", "Max per payment"],
  ["approval_above", "Ask me above"],
  ["max_per_day", "Max per 24 h"],
] as const;

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

  const changed = !!data && !!draft && JSON.stringify(draft) !== JSON.stringify(data.limits);

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
      setSaveError(message(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section>
      <Head title="Limits">
        Only you change these; the agent can't. Lowering a limit is always free
        {codes ? "; raising one needs a code from your authenticator app" : ""}.
      </Head>
      <Status error={error} loading={!data} />
      {notice && <Banner>{notice}</Banner>}
      {draft && (
        <form onSubmit={save}>
          {draft.map((l) => (
            <div key={l.asset} className="card limit-card">
              <div className="ticket-top">
                <span>{l.asset}</span>
              </div>
              <div className="details">
                {LIMIT_FIELDS.map(([field, label]) => (
                  <div key={field} className="detail">
                    <span>{label}</span>
                    <label>
                      <input
                        inputMode="decimal"
                        value={l[field]}
                        onChange={(e) => edit(l.asset, field, e.target.value)}
                        aria-label={`${label}, ${l.asset}`}
                      />
                      <span className="unit">{l.asset}</span>
                    </label>
                  </div>
                ))}
              </div>
            </div>
          ))}
          {changed && (
            <div className="save-bar">
              {codes && (
                <label className="field">
                  <span>Code, if you raised a limit</span>
                  <CodeInput value={code} onChange={setCode} invalid={!!saveError} />
                </label>
              )}
              {saveError && <p className="error">{saveError}</p>}
              <div className="actions">
                <button type="button" className="ghost" onClick={() => setData({ ...data! })}>
                  Discard
                </button>
                <button className="primary" disabled={saving}>
                  {saving ? "Saving…" : `Save as v${(data?.version ?? 0) + 1}`}
                </button>
              </div>
            </div>
          )}
        </form>
      )}
      {data && (
        <>
          <h3>History</h3>
          <div className="list">
            {data.versions.map((v) => (
              <div key={v.version} className="row">
                <span className="avatar">
                  <Icon name="limits" />
                </span>
                <div className="row-main">
                  <div className="row-title">Version {v.version}</div>
                  <div className="row-sub">
                    {when(v.created_at)} · {SOURCE_LABEL[v.source]}
                  </div>
                </div>
                {v.version === data.version && (
                  <span className="pill confirmed">
                    <Icon name="check" />
                    Current
                  </span>
                )}
              </div>
            ))}
          </div>
        </>
      )}
    </section>
  );
}

const SOURCE_LABEL = {
  config: "agile-money.toml",
  dashboard: "Dashboard",
  contacts: "Contacts changed",
} as const;

const CONTACT_FROM: Record<ContactRow["source"], string> = {
  config: "agile-money.toml",
  telegram: "Telegram",
  dashboard: "Dashboard",
  agent: "Suggested by Agile Money",
};

function Contacts({ onSetUp }: { onSetUp: () => void }) {
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
      setFormError(message(err));
    } finally {
      setBusy(false);
    }
  };

  const change = async (row: ContactRow, action: "cancel" | "remove") => {
    const question =
      action === "remove"
        ? `Remove ${row.label}? Agile Money won't be able to pay them until you add them again.`
        : `Cancel ${row.label}?`;
    if (!confirm(question)) return;
    try {
      setData(await api.changeContact(row.id, action));
    } catch (err) {
      setNotice(message(err));
      void reload();
    }
  };

  return (
    <section>
      <Head title="Contacts">
        The only people Agile Money can pay. A new or changed address waits{" "}
        {data?.cooling_off ?? "a while"} before it can be paid, and the first payment to anyone
        always asks you. Removing is instant.
      </Head>
      {notice && <Banner>{notice}</Banner>}
      <Status
        error={error}
        loading={!data}
        empty={data?.contacts.length === 0}
        emptyText="No contacts yet."
        emptyIcon="contacts"
      />
      {data && data.contacts.length > 0 && (
        <div className="list">
          {data.contacts.map((row) => (
            <div key={row.id} className="row">
              <Avatar name={row.label} />
              <div className="row-main">
                <div className="row-title">{row.label}</div>
                <div className="row-sub">
                  <span className="mono" title={row.address}>
                    {row.check}
                  </span>
                  {" · "}
                  {CONTACT_FROM[row.source]}
                </div>
              </div>
              <div className="row-side">
                <ContactState row={row} />
                <button
                  className="ghost small"
                  onClick={() => change(row, row.state === "active" ? "remove" : "cancel")}
                >
                  {row.state === "active" ? "Remove" : "Cancel"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
      <h3>Add or change a contact</h3>
      {data && !data.codes ? (
        <Banner
          icon="security"
          action={
            <button className="small primary" onClick={onSetUp}>
              Set up
            </button>
          }
        >
          Adding contacts needs authenticator codes. Until then they come from agile-money.toml only.
        </Banner>
      ) : (
        <form className="card form contact-form" onSubmit={add}>
          <label className="field">
            <span>Name</span>
            <input value={label} maxLength={40} onChange={(e) => setLabel(e.target.value)} />
          </label>
          <label className="field">
            <span>Address</span>
            <input
              className="mono"
              value={address}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setAddress(e.target.value)}
            />
            {address.trim().length > 14 ? (
              <p className="hint check-hint">
                Check it starts and ends as you expect: <strong>{short(address.trim())}</strong>
              </p>
            ) : (
              <p className="hint">Using a saved name replaces that contact's address after the wait.</p>
            )}
          </label>
          <label className="field">
            <span>Authenticator code</span>
            <CodeInput value={code} onChange={setCode} invalid={!!formError} />
          </label>
          {formError && <p className="error">{formError}</p>}
          <button
            className="primary wide"
            disabled={busy || !label.trim() || !address.trim() || code.length < 6}
          >
            {busy ? "Saving…" : "Save contact"}
          </button>
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
  if (row.state === "active")
    return (
      <span className="pill confirmed">
        <Icon name="check" />
        Can be paid
      </span>
    );
  if (row.state === "proposed") return <span className="pill open">Needs your code</span>;
  const left = row.active_at ? Math.max(0, new Date(row.active_at).getTime() - now) : 0;
  const hours = Math.floor(left / 3600000);
  const minutes = Math.ceil((left % 3600000) / 60000);
  return (
    <span className="pill open" title={row.replaces ? "Address changing" : undefined}>
      <Icon name="clock" />
      {left === 0 ? "Any moment" : `${hours ? `${hours} h ` : ""}${minutes} min`}
    </span>
  );
}

/** Enrols the owner's authenticator app, once. Replacing it is done on the server. */
function Security({
  overview,
  onChange,
}: {
  overview: Overview;
  onChange: (o: Overview) => void;
}) {
  const [setup, setSetup] = useState<CodeSetup | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [done, setDone] = useState(false);

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      setSetup(await api.codesSetup());
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  const confirmCode = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      onChange(await api.codesConfirm(code));
      setSetup(null);
      setCode("");
      setDone(true);
    } catch (err) {
      setError(message(err));
      if (err instanceof ApiError && err.status === 409) setSetup(null);
    } finally {
      setBusy(false);
    }
  };
  const copy = async () => {
    if (!setup) return;
    await navigator.clipboard.writeText(setup.key).catch(() => undefined);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <section>
      <Head title="Security">
        A code from your authenticator app guards anything that loosens what Agile Money may pay: new
        contacts, higher limits and resuming. Tightening never needs one.
      </Head>
      {done && <Banner>Codes are on. Telegram has been told.</Banner>}
      {overview.codes ? (
        <div className="card">
          <div className="ticket-top">
            <span>
              <Icon name="key" />
              Authenticator
            </span>
            <span className="pill confirmed">
              <Icon name="check" />
              On
            </span>
          </div>
          <div className="details">
            <Detail label="Asked for">Contacts, raising limits, resuming, signing in</Detail>
            <Detail label="Wrong codes">5 in 15 minutes locks codes for 15 minutes</Detail>
            <Detail label="New device">
              Remove the old secret on the server, then set up again here
            </Detail>
          </div>
        </div>
      ) : !setup ? (
        <div className="card form">
          <div className="ticket-top">
            <span>
              <Icon name="key" />
              Authenticator
            </span>
            <span className="pill open">Off</span>
          </div>
          <p className="muted" style={{ margin: 0 }}>
            Works with 1Password, Authy, Google Authenticator or any app that shows 6-digit codes.
            Once on, it can only be replaced from the server.
          </p>
          {error && <p className="error">{error}</p>}
          <button className="primary wide" onClick={start} disabled={busy}>
            {busy ? "Starting…" : "Set up authenticator"}
          </button>
        </div>
      ) : (
        <form className="card enrol" onSubmit={confirmCode}>
          <img
            className="qr"
            src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(setup.qr)}`}
            alt="QR code for your authenticator app"
          />
          <div className="form">
            <ol className="steps">
              <li>Scan the code with your authenticator app.</li>
              <li>Or type this key in by hand.</li>
              <li>Enter the 6 digits it shows to finish.</li>
            </ol>
            <div className="key">
              <span>{setup.key.replace(/(.{4})/g, "$1 ").trim()}</span>
              <button type="button" className="icon-btn" onClick={copy} aria-label="Copy key">
                <Icon name={copied ? "check" : "copy"} />
              </button>
            </div>
            <label className="field">
              <span>Code from the app</span>
              <CodeInput value={code} onChange={setCode} autoFocus invalid={!!error} />
            </label>
            {error && <p className="error">{error}</p>}
            <div className="actions split">
              <button
                type="button"
                className="ghost"
                onClick={() => {
                  setSetup(null);
                  setCode("");
                  setError(null);
                }}
              >
                Cancel
              </button>
              <button className="primary" disabled={busy || code.length < 6}>
                {busy ? "Checking…" : "Turn on codes"}
              </button>
            </div>
          </div>
        </form>
      )}
    </section>
  );
}

/** Who the owner is: the Telegram chat that answers, and wallets that sign in. */
function ProfilePage({ onSignOut, onSetUp }: { onSignOut: () => void; onSetUp: () => void }) {
  const { data, error, setData, reload } = usePoll(api.profile, POLL_MS * 2);
  const [linking, setLinking] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const openTelegram = async () => {
    // Opened before the request resolves, so the browser doesn't block the tab.
    const tab = window.open("", "_blank");
    try {
      const { url } = await api.telegramLink();
      if (tab) tab.location.href = url;
      else location.href = url;
      setNotice("Tap Start in Telegram. This page updates when it's done.");
      setTimeout(() => void reload(), 4000);
    } catch (err) {
      tab?.close();
      setNotice(message(err));
    }
  };
  const remove = async (address: string, label: string) => {
    if (!confirm(`Stop ${label} from signing in?`)) return;
    try {
      setData(await api.unlinkWallet(address));
    } catch (err) {
      setNotice(message(err));
    }
  };

  const t = data?.telegram;
  return (
    <section>
      <Head title="Profile">How the dashboard knows it's you.</Head>
      {notice && <Banner icon="info">{notice}</Banner>}
      <Status error={error} loading={!data} />
      {data && t && (
        <>
          <div className="card">
            <div className="ticket-top">
              <span>
                <Icon name="send" />
                Telegram
              </span>
              {t.linked_at ? (
                <span className="pill confirmed">
                  <Icon name="check" />
                  Linked
                </span>
              ) : (
                <span className="pill open">Not confirmed</span>
              )}
            </div>
            <div className="details">
              <Detail label="Account">
                {t.username ? `@${t.username}` : (t.name ?? "Owner chat")}
              </Detail>
              <Detail label="Chat id">
                <span className="mono">{t.chat_id}</span>
              </Detail>
              {t.linked_at && <Detail label="Confirmed">{when(t.linked_at)}</Detail>}
              {t.bot && (
                <Detail label="Bot">
                  <a href={`https://t.me/${t.bot}`} target="_blank" rel="noreferrer">
                    @{t.bot}
                  </a>
                </Detail>
              )}
            </div>
            <p className="hint">
              Approvals and alerts go to the chat set in agile-money.toml. Linking confirms this
              account is that chat; other chats can't link.
            </p>
            <button className="wide" onClick={openTelegram}>
              <Icon name="send" />
              {t.linked_at ? "Confirm again in Telegram" : "Link in Telegram"}
            </button>
          </div>

          <h3>Sign-in wallets</h3>
          <div className="card">
            {data.wallets.length === 0 ? (
              <p className="muted" style={{ margin: "0 0 12px" }}>
                Link a Solana wallet to sign in with it instead of the password.
                {data.codes ? " You'll still enter your authenticator code." : ""}
              </p>
            ) : (
              <div className="list flat">
                {data.wallets.map((w) => (
                  <div key={w.address} className="row">
                    <span className="avatar">
                      <Icon name="wallet" />
                    </span>
                    <div className="row-main">
                      <div className="row-title">{w.label}</div>
                      <div className="row-sub">
                        <span className="mono" title={w.address}>
                          {short(w.address)}
                        </span>
                        {" · "}
                        {when(w.linked_at)}
                      </div>
                    </div>
                    <button className="ghost small" onClick={() => remove(w.address, w.label)}>
                      Remove
                    </button>
                  </div>
                ))}
              </div>
            )}
            {!data.codes ? (
              <Banner
                icon="security"
                action={
                  <button className="small primary" onClick={onSetUp}>
                    Set up
                  </button>
                }
              >
                Linking a wallet needs authenticator codes, so a password alone can't add a way in.
              </Banner>
            ) : linking ? (
              <LinkWallet
                onDone={(p) => {
                  setData(p);
                  setLinking(false);
                  setNotice("Wallet linked. Telegram has been told.");
                }}
                onCancel={() => setLinking(false)}
              />
            ) : (
              <button className="wide" onClick={() => setLinking(true)}>
                <Icon name="plus" />
                Link a wallet
              </button>
            )}
          </div>
        </>
      )}
      <button className="ghost wide sign-out" onClick={onSignOut}>
        <Icon name="logout" />
        Sign out
      </button>
    </section>
  );
}

/** Pick a wallet, name it, give a code; the wallet then signs the link message. */
function LinkWallet({
  onDone,
  onCancel,
}: {
  onDone: (p: Profile) => void;
  onCancel: () => void;
}) {
  const [label, setLabel] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const link = async (wallet: Wallet) => {
    setBusy(true);
    setError(null);
    try {
      const address = await connect(wallet);
      const { message } = await api.linkChallenge(address);
      const signature = await signText(wallet, address, message);
      onDone(await api.linkWallet(address, signature, label.trim() || wallet.name, code));
    } catch (err) {
      if (!declined(err)) setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="form link-wallet">
      <label className="field">
        <span>Name</span>
        <input
          value={label}
          maxLength={40}
          placeholder="Phone, laptop…"
          onChange={(e) => setLabel(e.target.value)}
        />
      </label>
      <label className="field">
        <span>Authenticator code</span>
        <CodeInput value={code} onChange={setCode} invalid={!!error} />
        <p className="hint">Then pick the wallet; it asks you to sign a message, not a payment.</p>
      </label>
      {error && <p className="error">{error}</p>}
      {code.length === 6 ? (
        <WalletButtons busy={busy} label="Link" onPick={link} />
      ) : (
        <button className="wide" disabled>
          Enter the code to continue
        </button>
      )}
      <button type="button" className="ghost wide" onClick={onCancel}>
        Cancel
      </button>
    </div>
  );
}

/** Six digits from the owner's authenticator app. */
function CodeInput({
  value,
  onChange,
  autoFocus,
  invalid,
}: {
  value: string;
  onChange: (code: string) => void;
  autoFocus?: boolean;
  invalid?: boolean;
}) {
  return (
    <input
      className="code"
      inputMode="numeric"
      autoComplete="one-time-code"
      maxLength={6}
      placeholder="000000"
      aria-label="Authenticator code"
      aria-invalid={invalid}
      autoFocus={autoFocus}
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
  emptyIcon = "check",
}: {
  error: string | null;
  loading: boolean;
  empty?: boolean;
  emptyText?: string;
  emptyIcon?: IconName;
}) {
  if (error) return <Banner error>{error}</Banner>;
  if (loading) return <p className="muted">Loading…</p>;
  if (empty)
    return (
      <div className="empty">
        <Icon name={emptyIcon} />
        {emptyText}
      </div>
    );
  return null;
}

function Head({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="section-head">
      <h2>{title}</h2>
      <p>{children}</p>
    </div>
  );
}

function Avatar({ name }: { name: string }) {
  return (
    <span className="avatar" aria-hidden>
      {name.trim().charAt(0).toUpperCase() || "?"}
    </span>
  );
}

function short(address: string): string {
  return address.length > 14 ? `${address.slice(0, 4)}…${address.slice(-4)}` : address;
}

function when(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function time(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

/** Groups rows under "Today", "Yesterday" or the date, keeping their order. */
function byDay(rows: IntentRow[]): [string, IntentRow[]][] {
  const label = (iso: string) => {
    const d = new Date(iso);
    if (d.toDateString() === new Date().toDateString()) return "Today";
    if (d.toDateString() === new Date(Date.now() - 86_400_000).toDateString()) return "Yesterday";
    return d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
  };
  const groups: [string, IntentRow[]][] = [];
  for (const row of rows) {
    const day = label(row.created_at);
    const last = groups[groups.length - 1];
    if (last && last[0] === day) last[1].push(row);
    else groups.push([day, [row]]);
  }
  return groups;
}
