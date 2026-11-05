import { useOverview } from "../api/queries";
import { Banner } from "../components/Banner";
import { Icon } from "../components/Icon";
import { errorText, short } from "../lib/format";
import { Activity } from "../views/Activity";
import { Approvals } from "../views/Approvals";
import { Contacts } from "../views/Contacts";
import { Limits } from "../views/Limits";
import { Profile } from "../views/Profile";
import { Requests } from "../views/Requests";
import { Security } from "../views/Security";
import { TABS, useTab } from "./navigation";
import { StatusStrip } from "./StatusStrip";

/** The signed-in frame: top bar, status, banners and the open page. */
export function Shell() {
  const [tab, go] = useTab();
  const overview = useOverview();
  const o = overview.data;
  const setUp = () => go("security");

  return (
    <div className="shell">
      <header className="topbar">
        <nav className="tabs" aria-label="Pages">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={tab === t.id ? "tab active" : "tab"}
              onClick={() => go(t.id)}
              aria-current={tab === t.id ? "page" : undefined}
            >
              <Icon name={t.icon} />
              {t.label}
              {t.id === "approvals" && !!o?.awaiting_approval && (
                <span className="count">{o.awaiting_approval}</span>
              )}
            </button>
          ))}
        </nav>
        <span className="spacer" />
        {o && (
          <span className="wallet-chip" title={o.wallet}>
            {short(o.wallet)}
            <span className="net">{o.cluster}</span>
          </span>
        )}
        <button
          className={tab === "profile" ? "icon-btn active" : "icon-btn"}
          onClick={() => go("profile")}
          title="Profile"
          aria-label="Profile"
        >
          <Icon name="profile" />
        </button>
      </header>
      {overview.error && <Banner error>{errorText(overview.error)}</Banner>}
      {o && <StatusStrip overview={o} />}
      {o && !o.codes && tab !== "security" && (
        <Banner
          icon="info"
          action={
            <button className="small primary" onClick={setUp}>
              Set up
            </button>
          }
        >
          Authenticator codes are off, so the password alone can raise limits and add contacts.
        </Banner>
      )}
      <main>
        {tab === "approvals" && <Approvals />}
        {tab === "activity" && <Activity />}
        {tab === "requests" && <Requests />}
        {tab === "contacts" && <Contacts onSetUp={setUp} />}
        {tab === "limits" && <Limits />}
        {tab === "security" && <Security />}
        {tab === "profile" && <Profile onSetUp={setUp} />}
      </main>
    </div>
  );
}
