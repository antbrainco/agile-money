import { useState, type SubmitEvent } from "react";
import { ApiError } from "../api/client";
import { useCodesConfirm, useCodesSetup, useOverview } from "../api/queries";
import { Banner } from "../components/Banner";
import { CodeInput } from "../components/CodeInput";
import { Detail } from "../components/Detail";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { errorText } from "../lib/format";

/** Enrols the owner's authenticator app, once. Replacing it is done on the server. */
export function Security() {
  const overview = useOverview();
  // The enrolment secret lives only in this mutation's result, never in the query cache.
  const setup = useCodesSetup();
  const confirm = useCodesConfirm();
  const [code, setCode] = useState("");
  const [copied, setCopied] = useState(false);
  const [done, setDone] = useState(false);

  const cancel = () => {
    setup.reset();
    confirm.reset();
    setCode("");
  };

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    confirm.mutate(code, {
      onSuccess: () => {
        setup.reset();
        setCode("");
        setDone(true);
      },
      // The setup expired or was replaced: start again.
      onError: (err) => err instanceof ApiError && err.status === 409 && setup.reset(),
    });
  };

  const copy = async () => {
    if (!setup.data) return;
    await navigator.clipboard.writeText(setup.data.key).catch(() => undefined);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const enrol = setup.data;
  const error = setup.error ?? confirm.error;
  return (
    <section>
      <Head title="Security">
        A code from your authenticator app guards anything that loosens what Agile Money may pay: new
        contacts, higher limits and resuming. Tightening never needs one.
      </Head>
      {done && <Banner>Codes are on. Telegram has been told.</Banner>}
      <Status error={null} loading={!overview.data} />
      {overview.data?.codes ? (
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
      ) : overview.data && !enrol ? (
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
          {error && <p className="error">{errorText(error)}</p>}
          <button className="primary wide" onClick={() => setup.mutate()} disabled={setup.isPending}>
            {setup.isPending ? "Starting…" : "Set up authenticator"}
          </button>
        </div>
      ) : enrol ? (
        <form className="card enrol" onSubmit={submit}>
          <img
            className="qr"
            src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(enrol.qr)}`}
            alt="QR code for your authenticator app"
          />
          <div className="form">
            <ol className="steps">
              <li>Scan the code with your authenticator app.</li>
              <li>Or type this key in by hand.</li>
              <li>Enter the 6 digits it shows to finish.</li>
            </ol>
            <div className="key">
              <span>{enrol.key.replace(/(.{4})/g, "$1 ").trim()}</span>
              <button type="button" className="icon-btn" onClick={copy} aria-label="Copy key">
                <Icon name={copied ? "check" : "copy"} />
              </button>
            </div>
            <label className="field">
              <span>Code from the app</span>
              <CodeInput value={code} onChange={setCode} autoFocus invalid={!!confirm.error} />
            </label>
            {confirm.error && <p className="error">{errorText(confirm.error)}</p>}
            <div className="actions split">
              <button type="button" className="ghost" onClick={cancel}>
                Cancel
              </button>
              <button className="primary" disabled={confirm.isPending || code.length < 6}>
                {confirm.isPending ? "Checking…" : "Turn on codes"}
              </button>
            </div>
          </div>
        </form>
      ) : null}
    </section>
  );
}
