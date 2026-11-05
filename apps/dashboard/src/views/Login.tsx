import { useState, type SubmitEvent } from "react";
import { useLogin, useWalletLogin } from "../api/queries";
import { CodeInput } from "../components/CodeInput";
import { Icon } from "../components/Icon";
import { WalletButtons } from "../components/WalletButtons";
import { errorText } from "../lib/format";
import { declined } from "../lib/wallet";

export function Login() {
  const login = useLogin();
  const walletLogin = useWalletLogin();
  const [password, setPassword] = useState("");
  const [show, setShow] = useState(false);
  const [code, setCode] = useState("");

  const busy = login.isPending || walletLogin.isPending;
  // Closing the wallet's prompt isn't an error worth showing.
  const error = login.error ?? (declined(walletLogin.error) ? null : walletLogin.error);

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    walletLogin.reset();
    login.mutate({ password, code: code || undefined });
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
          {error && <p className="error">{errorText(error)}</p>}
          <button className="primary wide" disabled={busy || !password}>
            {login.isPending ? "Signing in…" : "Sign in"}
          </button>
          <div className="or">or</div>
          <WalletButtons
            busy={busy}
            label="Continue with"
            onPick={(wallet) => {
              login.reset();
              walletLogin.mutate({ wallet, code: code || undefined });
            }}
          />
        </div>
      </form>
    </main>
  );
}
