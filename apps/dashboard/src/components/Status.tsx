import { errorText } from "../lib/format";
import { Banner } from "./Banner";
import { Icon, type IconName } from "./Icon";

/** A query's error, loading or empty state; nothing once there is data to show. */
export function Status({
  error,
  loading,
  empty,
  emptyText,
  emptyIcon = "check",
}: {
  error: unknown;
  loading: boolean;
  empty?: boolean;
  emptyText?: string;
  emptyIcon?: IconName;
}) {
  if (error) return <Banner error>{errorText(error)}</Banner>;
  if (loading) return <p className="muted">Loading…</p>;
  if (empty)
    return (
      <div className="empty">
        <Icon name={emptyIcon} />
        {emptyText}
      </div>
    );
  return null;
}
