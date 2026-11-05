import { useEffect, useState } from "react";
import { onWallets, wallets, type Wallet } from "../lib/wallet";

/** The wallets this browser has, updated as extensions announce themselves. */
function useWallets(): Wallet[] {
  const [list, setList] = useState<Wallet[]>(wallets);
  useEffect(() => onWallets(() => setList(wallets())), []);
  return list;
}

/** One button per wallet the browser has; a hint when there are none. */
export function WalletButtons({
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
