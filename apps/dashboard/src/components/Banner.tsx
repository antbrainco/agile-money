import type { ReactNode } from "react";
import { Icon, type IconName } from "./Icon";

export function Banner({
  children,
  error,
  icon,
  action,
}: {
  children: ReactNode;
  error?: boolean;
  icon?: IconName;
  action?: ReactNode;
}) {
  return (
    <div className={error ? "banner error" : "banner"} role={error ? "alert" : "status"}>
      <Icon name={icon ?? (error ? "info" : "check")} />
      <span>{children}</span>
      {action}
    </div>
  );
}

/** The nudge shown wherever something needs authenticator codes first. */
export function SetUpCodes({ children, onSetUp }: { children: ReactNode; onSetUp: () => void }) {
  return (
    <Banner
      icon="security"
      action={
        <button className="small primary" onClick={onSetUp}>
          Set up
        </button>
      }
    >
      {children}
    </Banner>
  );
}
