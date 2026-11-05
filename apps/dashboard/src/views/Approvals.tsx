import { useState } from "react";
import { useAnswer, useApprovals } from "../api/queries";
import type { IntentRow } from "../api/types";
import { Avatar } from "../components/Avatar";
import { Banner } from "../components/Banner";
import { Expiry } from "../components/Expiry";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { errorText, short } from "../lib/format";

export function Approvals() {
  const approvals = useApprovals();
  const answer = useAnswer();
  const [notice, setNotice] = useState<string | null>(null);

  const reply = (row: IntentRow, action: "approve" | "reject") => {
    if (!row.approval) return;
    answer.mutate(
      { approval: row.approval.id, action },
      {
        onSuccess: ({ message }) =>
          setNotice(`${row.money.amount} ${row.money.asset} to ${row.to}: ${message}`),
        // Usually "Already answered." from Telegram first: the first answer wins.
        onError: (err) => setNotice(errorText(err)),
      },
    );
  };
  const answering = (row: IntentRow) =>
    answer.isPending && answer.variables?.approval === row.approval?.id;

  const data = approvals.data;
  return (
    <section>
      <Head title="Approvals">
        Answer here or in Telegram. Whichever comes first counts; the other closes.
      </Head>
      {notice && <Banner>{notice}</Banner>}
      <Status
        error={approvals.error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="Nothing waiting for you."
        emptyIcon="approvals"
      />
      <div className="cards">
        {data?.map((row) => (
          <article key={row.id} className="card approval">
            <div className="approval-to">
              <Avatar name={row.to} />
              <div className="row-main">
                <div className="row-title">{row.to}</div>
                <div className="row-sub mono" title={row.address}>
                  {short(row.address)}
                </div>
              </div>
            </div>
            <div className="amount">
              {row.money.amount}
              <span className="unit">{row.money.asset}</span>
            </div>
            {row.memo && <div className="memo">“{row.memo}”</div>}
            {row.reasons.length > 0 && (
              <ul className="reasons">
                {row.reasons.map((r) => (
                  <li key={r}>
                    <Icon name="info" />
                    {r}
                  </li>
                ))}
              </ul>
            )}
            {row.approval && <Expiry at={row.approval.expires_at} />}
            <div className="actions split">
              <button className="ghost" disabled={answering(row)} onClick={() => reply(row, "reject")}>
                Reject
              </button>
              <button
                className="primary"
                disabled={answering(row)}
                onClick={() => reply(row, "approve")}
              >
                <Icon name="check" />
                Approve
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
