// Client state: which page is open. It lives in the URL hash, so reload and back keep it.

import { useEffect, useState } from "react";
import type { IconName } from "../components/Icon";

export type Tab =
  | "approvals"
  | "activity"
  | "requests"
  | "contacts"
  | "limits"
  | "security"
  | "profile";

/** The pages in the top bar. Profile has its own button. */
export const TABS: { id: Tab; label: string; icon: IconName }[] = [
  { id: "approvals", label: "Approvals", icon: "approvals" },
  { id: "activity", label: "Activity", icon: "activity" },
  { id: "requests", label: "Requests", icon: "requests" },
  { id: "contacts", label: "Contacts", icon: "contacts" },
  { id: "limits", label: "Limits", icon: "limits" },
  { id: "security", label: "Security", icon: "security" },
];

function fromHash(): Tab {
  const id = location.hash.replace(/^#\/?/, "");
  return id === "profile" || TABS.some((t) => t.id === id) ? (id as Tab) : "approvals";
}

export function useTab(): [Tab, (t: Tab) => void] {
  const [tab, setTab] = useState<Tab>(fromHash);
  useEffect(() => {
    const onHash = () => setTab(fromHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const go = (t: Tab) => {
    if (t !== tab) location.hash = `/${t}`;
    window.scrollTo({ top: 0 });
  };
  return [tab, go];
}
