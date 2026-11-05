import { useState, type SubmitEvent } from "react";
import { useOverview, usePolicy, useSavePolicy } from "../api/queries";
import type { Limits as AssetLimits } from "../api/types";
import { Banner } from "../components/Banner";
import { CodeInput } from "../components/CodeInput";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { errorText, when } from "../lib/format";

const FIELDS = [
  ["max_per_tx", "Max per payment"],
  ["approval_above", "Ask me above"],
  ["max_per_day", "Max per 24 h"],
] as const;

const SOURCE_LABEL = {
  config: "agile-money.toml",
  dashboard: "Dashboard",
  contacts: "Contacts changed",
} as const;

export function Limits() {
  const policy = usePolicy();
  const codes = !!useOverview().data?.codes;
  const save = useSavePolicy();
  // The owner's unsaved edits; null means none, so the saved limits show.
  const [draft, setDraft] = useState<AssetLimits[] | null>(null);
  const [code, setCode] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const data = policy.data;
  const limits = draft ?? data?.limits;
  const changed = !!data && !!draft && JSON.stringify(draft) !== JSON.stringify(data.limits);

  const edit = (asset: string, field: keyof AssetLimits, value: string) =>
    setDraft((d) => (d ?? data!.limits).map((l) => (l.asset === asset ? { ...l, [field]: value } : l)));

  const discard = () => {
    setDraft(null);
    setCode("");
    save.reset();
  };

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    if (!data || !draft) return;
    save.mutate(
      { version: data.version, limits: draft, code: code || undefined },
      {
        onSuccess: (next) => {
          setDraft(null);
          setCode("");
          setNotice(`Saved as version ${next.version}. It applies to the next payment.`);
        },
      },
    );
  };

  return (
    <section>
      <Head title="Limits">
        Only you change these; the agent can't. Lowering a limit is always free
        {codes ? "; raising one needs a code from your authenticator app" : ""}.
      </Head>
      <Status error={policy.error} loading={!data} />
      {notice && <Banner>{notice}</Banner>}
      {limits && (
        <form onSubmit={submit}>
          {limits.map((l) => (
            <div key={l.asset} className="card limit-card">
              <div className="ticket-top">
                <span>{l.asset}</span>
              </div>
              <div className="details">
                {FIELDS.map(([field, label]) => (
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
                  <CodeInput value={code} onChange={setCode} invalid={!!save.error} />
                </label>
              )}
              {save.error && <p className="error">{errorText(save.error)}</p>}
              <div className="actions">
                <button type="button" className="ghost" onClick={discard}>
                  Discard
                </button>
                <button className="primary" disabled={save.isPending}>
                  {save.isPending ? "Saving…" : `Save as v${data.version + 1}`}
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
