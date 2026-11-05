import { useSession } from "../api/queries";
import { Login } from "../views/Login";
import { Shell } from "./Shell";

/** Signed in or not. A read that comes back 401 flips this, from the query client. */
export function App() {
  const session = useSession();
  if (!session.data) return <main className="center muted">Loading…</main>;
  return session.data.signedIn ? <Shell /> : <Login />;
}
