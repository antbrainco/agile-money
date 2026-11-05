// Server state: everything the dashboard shows from the bot lives in the query cache,
// keyed below. Views read it through these hooks and change it through the mutations,
// which update or refetch exactly the entries they affect. Nothing else holds a copy.

import {
  QueryCache,
  QueryClient,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { connect, signText, type Wallet } from "../lib/wallet";
import { api, ApiError } from "./client";
import type { Limits } from "./types";

export const keys = {
  session: ["session"],
  overview: ["overview"],
  approvals: ["approvals"],
  intents: ["intents"],
  invoices: ["invoices"],
  policy: ["policy"],
  contacts: ["contacts"],
  profile: ["profile"],
} as const;

/** Approvals expire in 15 minutes, so poll often enough to matter. */
const FAST = 5_000;
const SLOW = 10_000;

export function createQueryClient(): QueryClient {
  const client: QueryClient = new QueryClient({
    // A 401 on a read means the session is gone (expired, or the bot token rotated).
    // Writes can 401 for a wrong authenticator code, so only reads sign the owner out.
    queryCache: new QueryCache({
      onError: (err, query) => {
        if (err instanceof ApiError && err.status === 401 && query.queryKey[0] !== "session") {
          client.setQueryData(keys.session, { signedIn: false });
        }
      },
    }),
    defaultOptions: {
      queries: {
        // Retry only what might pass next time: network errors and 5xx.
        retry: (count, err) => count < 2 && !(err instanceof ApiError && err.status < 500),
      },
    },
  });
  return client;
}

// Reads.

export const useSession = () =>
  useQuery({
    queryKey: keys.session,
    queryFn: () => api.session().catch(() => ({ signedIn: false })),
    staleTime: Infinity,
  });

export const useOverview = () =>
  useQuery({ queryKey: keys.overview, queryFn: api.overview, refetchInterval: FAST });

export const useApprovals = () =>
  useQuery({ queryKey: keys.approvals, queryFn: api.approvals, refetchInterval: FAST });

export const useIntents = () =>
  useQuery({ queryKey: keys.intents, queryFn: api.intents, refetchInterval: SLOW });

export const useInvoices = () =>
  useQuery({ queryKey: keys.invoices, queryFn: api.invoices, refetchInterval: SLOW });

/** Not polled: the owner may be editing it. */
export const usePolicy = () =>
  useQuery({ queryKey: keys.policy, queryFn: api.policy, refetchOnWindowFocus: false });

export const useContacts = () =>
  useQuery({ queryKey: keys.contacts, queryFn: api.contacts, refetchInterval: SLOW });

export const useProfile = () =>
  useQuery({ queryKey: keys.profile, queryFn: api.profile, refetchInterval: SLOW });

// Sign-in.

const signedIn = { signedIn: true };

export function useLogin() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ password, code }: { password: string; code?: string }) =>
      api.login(password, code),
    onSuccess: () => client.setQueryData(keys.session, signedIn),
  });
}

/** The wallet signs the bot's one-time message; the Worker then opens a session. */
export function useWalletLogin() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async ({ wallet, code }: { wallet: Wallet; code?: string }) => {
      const address = await connect(wallet);
      const { message } = await api.walletChallenge(address);
      const signature = await signText(wallet, address, message);
      return api.walletLogin(address, signature, code);
    },
    onSuccess: () => client.setQueryData(keys.session, signedIn),
  });
}

export function useSignOut() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: () => api.logout().catch(() => undefined),
    onSettled: () => {
      // Signed out first, so the views unmount before their data goes.
      client.setQueryData(keys.session, { signedIn: false });
      client.removeQueries({ predicate: (q) => q.queryKey[0] !== "session" });
    },
  });
}

// Payments.

export function useAnswer() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ approval, action }: { approval: string; action: "approve" | "reject" }) =>
      api.answer(approval, action),
    // Answered here or already in Telegram: either way the list has moved on.
    onSettled: () =>
      Promise.all(
        [keys.approvals, keys.overview, keys.intents].map((queryKey) =>
          client.invalidateQueries({ queryKey }),
        ),
      ),
  });
}

export function usePause() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ paused, code }: { paused: boolean; code?: string }) => api.pause(paused, code),
    onSuccess: (overview) => client.setQueryData(keys.overview, overview),
  });
}

export function useSavePolicy() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ version, limits, code }: { version: number; limits: Limits[]; code?: string }) =>
      api.savePolicy(version, limits, code),
    onSuccess: (policy) => {
      client.setQueryData(keys.policy, policy);
      void client.invalidateQueries({ queryKey: keys.overview });
    },
  });
}

// Contacts.

export function useAddContact() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ label, address, code }: { label: string; address: string; code: string }) =>
      api.addContact(label, address, code),
    onSuccess: (contacts) => client.setQueryData(keys.contacts, contacts),
  });
}

export function useChangeContact() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, action }: { id: string; action: "cancel" | "remove" }) =>
      api.changeContact(id, action),
    onSuccess: (contacts) => client.setQueryData(keys.contacts, contacts),
    onError: () => client.invalidateQueries({ queryKey: keys.contacts }),
  });
}

// Security.

/** Starts enrolment. The result holds the secret, so it stays in this mutation, uncached. */
export const useCodesSetup = () => useMutation({ mutationFn: api.codesSetup });

export function useCodesConfirm() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (code: string) => api.codesConfirm(code),
    onSuccess: (overview) => {
      client.setQueryData(keys.overview, overview);
      void client.invalidateQueries({ queryKey: keys.contacts });
      void client.invalidateQueries({ queryKey: keys.profile });
    },
  });
}

// Profile.

/** Pick a wallet; it signs the link message, then the bot records it. */
export function useLinkWallet() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async ({ wallet, label, code }: { wallet: Wallet; label: string; code: string }) => {
      const address = await connect(wallet);
      const { message } = await api.linkChallenge(address);
      const signature = await signText(wallet, address, message);
      return api.linkWallet(address, signature, label || wallet.name, code);
    },
    onSuccess: (profile) => client.setQueryData(keys.profile, profile),
  });
}

export function useUnlinkWallet() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (address: string) => api.unlinkWallet(address),
    onSuccess: (profile) => client.setQueryData(keys.profile, profile),
  });
}

export const useTelegramLink = () => useMutation({ mutationFn: api.telegramLink });
