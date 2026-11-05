import type { ReactNode } from "react";

export function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="detail">
      <span>{label}</span>
      <span>{children}</span>
    </div>
  );
}
