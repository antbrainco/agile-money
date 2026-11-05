import { useState } from "react";
import { useIntents } from "../api/queries";
import type { IntentRow } from "../api/types";
import { Avatar } from "../components/Avatar";
import { Detail } from "../components/Detail";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Sheet } from "../components/Sheet";
import { endedBadly, StatePill } from "../components/StatePill";
import { Status } from "../components/Status";
import { byDay, short, time, when } from "../lib/format";

export function Activity() {
  const intents = useIntents();
  const [open, setOpen] = useState<IntentRow | null>(null);
  const data = intents.data;
  return (
    <section>
      <Head title="Activity">Everything the agent proposed, newest first.</Head>
      <Status
        error={intents.error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="No payments yet."
        emptyIcon="activity"
      />
      {data && data.length > 0 && (
        <div className="list">
          {byDay(data).map(([day, rows]) => (
            <div key={day}>
              <div className="day">{day}</div>
              {rows.map((row) => {
                const note = why(row);
                const bad = endedBadly(row.state);
                return (
                  <button key={row.id} className="row-button" onClick={() => setOpen(row)}>
                    <Avatar name={row.to} />
                    <div className="row-main">
                      <div className="row-title">{row.to}</div>
                      <div className="row-sub">
                        {time(row.created_at)}
                        {row.memo && ` · ${row.memo}`}
                      </div>
                      {note && bad && <div className="row-note bad">{note}</div>}
                    </div>
                    <div className="row-side">
                      <span className={bad ? "row-amount void" : "row-amount"}>
                        −{row.money.amount} {row.money.asset}
                      </span>
                      <StatePill state={row.state} />
                    </div>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      )}
      {open && <Receipt row={open} onClose={() => setOpen(null)} />}
    </section>
  );
}

function Receipt({ row, onClose }: { row: IntentRow; onClose: () => void }) {
  const note = why(row);
  return (
    <Sheet title="Receipt" onClose={onClose}>
      <div className="ticket">
        <div className="ticket-top">
          <span>
            <Icon name="activity" />
            Payment
          </span>
          <StatePill state={row.state} />
        </div>
        <div className="ticket-cut" />
        <div className="ticket-hero">
          <span className="avatar large">
            <Icon name="store" />
          </span>
          <span className="who">{row.to}</span>
          <span className="amount">
            −{row.money.amount}
            <span className="unit">{row.money.asset}</span>
          </span>
        </div>
        <div className="details">
          <Detail label="Date & time">{when(row.created_at)}</Detail>
          <Detail label="To">
            <span className="mono" title={row.address}>
              {short(row.address)}
            </span>
          </Detail>
          {row.memo && <Detail label="Memo">{row.memo}</Detail>}
          {note && <Detail label="Note">{note}</Detail>}
          {row.link && (
            <Detail label="Transaction">
              <a href={row.link} target="_blank" rel="noreferrer">
                View in explorer
              </a>
            </Detail>
          )}
        </div>
      </div>
    </Sheet>
  );
}

/** One line on why a payment ended where it did. */
function why(row: IntentRow): string | null {
  if (row.state === "denied") return row.reasons.join("; ");
  if (row.approval?.outcome === "expired") return "Approval expired";
  if (row.approval?.outcome === "rejected") return "You rejected it";
  if (row.approval?.outcome === "granted") return "You approved it";
  if (row.failure && row.state !== "confirmed") return row.failure;
  return null;
}
