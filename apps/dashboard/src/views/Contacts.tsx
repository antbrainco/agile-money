import { useState, type SubmitEvent } from "react";
import { useAddContact, useChangeContact, useContacts } from "../api/queries";
import type { ContactRow } from "../api/types";
import { Avatar } from "../components/Avatar";
import { Banner, SetUpCodes } from "../components/Banner";
import { CodeInput } from "../components/CodeInput";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { errorText, short } from "../lib/format";
import { useNow } from "../lib/useNow";

const FROM: Record<ContactRow["source"], string> = {
  config: "agile-money.toml",
  telegram: "Telegram",
  dashboard: "Dashboard",
  agent: "Suggested by Agile Money",
};

export function Contacts({ onSetUp }: { onSetUp: () => void }) {
  const contacts = useContacts();
  const add = useAddContact();
  const change = useChangeContact();
  const [label, setLabel] = useState("");
  const [address, setAddress] = useState("");
  const [code, setCode] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const data = contacts.data;
  const wait = data?.cooling_off ?? "a while";

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const name = label.trim();
    add.mutate(
      { label: name, address: address.trim(), code },
      {
        onSuccess: () => {
          setNotice(`${name} is saved and can be paid in ${wait}. Telegram has a Cancel button until then.`);
          setLabel("");
          setAddress("");
          setCode("");
        },
      },
    );
  };

  const ask = (row: ContactRow) => {
    const action = row.state === "active" ? "remove" : "cancel";
    const question =
      action === "remove"
        ? `Remove ${row.label}? Agile Money won't be able to pay them until you add them again.`
        : `Cancel ${row.label}?`;
    if (!confirm(question)) return;
    change.mutate({ id: row.id, action }, { onError: (err) => setNotice(errorText(err)) });
  };

  return (
    <section>
      <Head title="Contacts">
        The only people Agile Money can pay. A new or changed address waits {wait} before it can be
        paid, and the first payment to anyone always asks you. Removing is instant.
      </Head>
      {notice && <Banner>{notice}</Banner>}
      <Status
        error={contacts.error}
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
                  {FROM[row.source]}
                </div>
              </div>
              <div className="row-side">
                <ContactState row={row} />
                <button className="ghost small" onClick={() => ask(row)}>
                  {row.state === "active" ? "Remove" : "Cancel"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
      <h3>Add or change a contact</h3>
      {data && !data.codes ? (
        <SetUpCodes onSetUp={onSetUp}>
          Adding contacts needs authenticator codes. Until then they come from agile-money.toml only.
        </SetUpCodes>
      ) : (
        <form className="card form contact-form" onSubmit={submit}>
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
            <CodeInput value={code} onChange={setCode} invalid={!!add.error} />
          </label>
          {add.error && <p className="error">{errorText(add.error)}</p>}
          <button
            className="primary wide"
            disabled={add.isPending || !label.trim() || !address.trim() || code.length < 6}
          >
            {add.isPending ? "Saving…" : "Save contact"}
          </button>
        </form>
      )}
    </section>
  );
}

/** Payable, waiting for a code, or counting down the cooling-off period. */
function ContactState({ row }: { row: ContactRow }) {
  const now = useNow(30_000, row.state === "confirmed");
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
