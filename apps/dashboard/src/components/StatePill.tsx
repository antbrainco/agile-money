import { Icon, type IconName } from "./Icon";

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

/** Payment states that end without money moving. */
export const endedBadly = (state: string) => ["denied", "rejected", "abandoned"].includes(state);

export function StatePill({ state }: { state: string }) {
  const icon: IconName = state === "confirmed" ? "check" : endedBadly(state) ? "close" : "clock";
  return (
    <span className={`pill ${state}`}>
      <Icon name={icon} />
      {STATE_LABEL[state] ?? state}
    </span>
  );
}
