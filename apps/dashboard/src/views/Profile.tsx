import { useState } from "react";
import {
  keys,
  useLinkWallet,
  useProfile,
  useSignOut,
  useTelegramLink,
  useUnlinkWallet,
} from "../api/queries";
import { useQueryClient } from "@tanstack/react-query";
import { Banner, SetUpCodes } from "../components/Banner";
import { CodeInput } from "../components/CodeInput";
import { Detail } from "../components/Detail";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { WalletButtons } from "../components/WalletButtons";
import { errorText, short, when } from "../lib/format";
import { declined } from "../lib/wallet";

/** Who the owner is: the Telegram chat that answers, and wallets that sign in. */
export function Profile({ onSetUp }: { onSetUp: () => void }) {
  const profile = useProfile();
  const client = useQueryClient();
  const telegramLink = useTelegramLink();
  const unlink = useUnlinkWallet();
  const signOut = useSignOut();
  const [linking, setLinking] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const openTelegram = () => {
    // Opened before the request resolves, so the browser doesn't block the tab.
    const tab = window.open("", "_blank");
    telegramLink.mutate(undefined, {
      onSuccess: ({ url }) => {
        if (tab) tab.location.href = url;
        else location.href = url;
        setNotice("Tap Start in Telegram. This page updates when it's done.");
        setTimeout(() => void client.invalidateQueries({ queryKey: keys.profile }), 4000);
      },
      onError: (err) => {
        tab?.close();
        setNotice(errorText(err));
      },
    });
  };

  const remove = (address: string, label: string) => {
    if (!confirm(`Stop ${label} from signing in?`)) return;
    unlink.mutate(address, { onError: (err) => setNotice(errorText(err)) });
  };

  const data = profile.data;
  const t = data?.telegram;
  return (
    <section>
      <Head title="Profile">How the dashboard knows it's you.</Head>
      {notice && <Banner icon="info">{notice}</Banner>}
      <Status error={profile.error} loading={!data} />
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
            <button className="wide" onClick={openTelegram} disabled={telegramLink.isPending}>
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
              <SetUpCodes onSetUp={onSetUp}>
                Linking a wallet needs authenticator codes, so a password alone can't add a way in.
              </SetUpCodes>
            ) : linking ? (
              <LinkWallet
                onDone={() => {
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
      <button
        className="ghost wide sign-out"
        onClick={() => signOut.mutate()}
        disabled={signOut.isPending}
      >
        <Icon name="logout" />
        Sign out
      </button>
    </section>
  );
}

/** Name it, give a code, pick the wallet; the wallet then signs the link message. */
function LinkWallet({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const link = useLinkWallet();
  const [label, setLabel] = useState("");
  const [code, setCode] = useState("");
  const error = declined(link.error) ? null : link.error;
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
      {error && <p className="error">{errorText(error)}</p>}
      {code.length === 6 ? (
        <WalletButtons
          busy={link.isPending}
          label="Link"
          onPick={(wallet) => link.mutate({ wallet, label: label.trim(), code }, { onSuccess: onDone })}
        />
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
