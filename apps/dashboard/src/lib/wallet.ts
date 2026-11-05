// Finds Solana wallets in the browser through the Wallet Standard (Phantom,
// Solflare, Backpack and others announce themselves this way) and has one sign
// a message. Nothing here sends a transaction.

interface StandardAccount {
  address: string;
  chains: readonly string[];
}

interface StandardWallet {
  name: string;
  icon: string;
  chains: readonly string[];
  accounts: readonly StandardAccount[];
  features: Record<string, unknown>;
}

interface ConnectFeature {
  connect(input?: { silent?: boolean }): Promise<{ accounts: readonly StandardAccount[] }>;
}

interface SignMessageFeature {
  signMessage(
    ...inputs: { account: StandardAccount; message: Uint8Array }[]
  ): Promise<readonly { signedMessage: Uint8Array; signature: Uint8Array }[]>;
}

export interface Wallet {
  name: string;
  icon: string;
  raw: StandardWallet;
}

const found = new Map<string, StandardWallet>();
const listeners = new Set<() => void>();

function add(wallet: StandardWallet) {
  const solana = wallet.chains.some((c) => c.startsWith("solana:"));
  if (!solana || !("solana:signMessage" in wallet.features)) return;
  if (!("standard:connect" in wallet.features)) return;
  found.set(wallet.name, wallet);
  listeners.forEach((l) => l());
}

let started = false;

/** Starts listening; wallets that loaded first answer the app-ready event. */
function start() {
  if (started || typeof window === "undefined") return;
  started = true;
  interface Register {
    register: (...wallets: StandardWallet[]) => () => void;
  }
  const api: Register = { register: (...wallets) => (wallets.forEach(add), () => {}) };
  window.addEventListener("wallet-standard:register-wallet", (event) => {
    (event as CustomEvent<(api: Register) => void>).detail(api);
  });
  window.dispatchEvent(
    new CustomEvent("wallet-standard:app-ready", { detail: api }),
  );
}

export function wallets(): Wallet[] {
  start();
  return [...found.values()].map((raw) => ({ name: raw.name, icon: raw.icon, raw }));
}

/** Calls `onChange` when a wallet appears; returns the unsubscribe. */
export function onWallets(onChange: () => void): () => void {
  start();
  listeners.add(onChange);
  return () => listeners.delete(onChange);
}

/** Asks the wallet for its account; the wallet shows its own prompt. */
export async function connect(wallet: Wallet): Promise<string> {
  const feature = wallet.raw.features["standard:connect"] as ConnectFeature;
  const { accounts } = await feature.connect();
  const account =
    accounts.find((a) => a.chains.some((c) => c.startsWith("solana:"))) ?? accounts[0];
  if (!account) throw new Error(`${wallet.name} shared no account.`);
  return account.address;
}

/** The wallet signs `message` as text; returns the signature in base58. */
export async function signText(wallet: Wallet, address: string, message: string): Promise<string> {
  const account = wallet.raw.accounts.find((a) => a.address === address);
  if (!account) throw new Error(`${wallet.name} isn't connected to that account.`);
  const feature = wallet.raw.features["solana:signMessage"] as SignMessageFeature;
  const [signed] = await feature.signMessage({
    account,
    message: new TextEncoder().encode(message),
  });
  if (!signed) throw new Error(`${wallet.name} didn't sign.`);
  return base58(signed.signature);
}

const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

export function base58(bytes: Uint8Array): string {
  const digits: number[] = [];
  for (const byte of bytes) {
    let carry = byte;
    for (let i = 0; i < digits.length; i++) {
      carry += digits[i]! << 8;
      digits[i] = carry % 58;
      carry = (carry / 58) | 0;
    }
    while (carry) {
      digits.push(carry % 58);
      carry = (carry / 58) | 0;
    }
  }
  let out = "";
  for (const byte of bytes) {
    if (byte !== 0) break;
    out += "1";
  }
  for (let i = digits.length - 1; i >= 0; i--) out += ALPHABET[digits[i]!];
  return out;
}

/** A user closing the wallet's prompt isn't an error worth showing in red. */
export function declined(err: unknown): boolean {
  const text = err instanceof Error ? err.message : String(err);
  return /reject|declin|cancel|denied|closed/i.test(text);
}
