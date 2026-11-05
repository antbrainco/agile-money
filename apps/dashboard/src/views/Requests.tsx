import { useInvoices } from "../api/queries";
import { Avatar } from "../components/Avatar";
import { Head } from "../components/Head";
import { Icon } from "../components/Icon";
import { Status } from "../components/Status";
import { when } from "../lib/format";

export function Requests() {
  const invoices = useInvoices();
  const data = invoices.data;
  return (
    <section>
      <Head title="Requests">Links Agile Money made for clients to pay you.</Head>
      <Status
        error={invoices.error}
        loading={!data}
        empty={data?.length === 0}
        emptyText="No requests yet."
        emptyIcon="requests"
      />
      {data && data.length > 0 && (
        <div className="list">
          {data.map((row) => (
            <div key={row.id} className="row">
              <Avatar name={row.client} />
              <div className="row-main">
                <div className="row-title">{row.client}</div>
                <div className="row-sub">
                  {when(row.created_at)} · {row.rail === "paypal" ? "PayPal" : "Solana Pay"}
                  {row.state === "open" && row.url.startsWith("https://") && (
                    <>
                      {" · "}
                      <a href={row.url} target="_blank" rel="noreferrer">
                        Link
                      </a>
                    </>
                  )}
                </div>
                {row.reason && <div className="row-note">{row.reason}</div>}
              </div>
              <div className="row-side">
                <span className={row.state === "cancelled" ? "row-amount void" : "row-amount"}>
                  +{row.money.amount} {row.money.asset}
                </span>
                <span className={`pill ${row.state}`}>
                  <Icon
                    name={row.state === "paid" ? "check" : row.state === "open" ? "clock" : "close"}
                  />
                  {row.state === "open" ? "Waiting" : row.state === "paid" ? "Paid" : "Cancelled"}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
