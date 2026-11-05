import { useState } from "react";
import { usePause } from "../api/queries";
import type { Overview } from "../api/types";
import { CodeInput } from "../components/CodeInput";
import { Icon } from "../components/Icon";
import { Sheet } from "../components/Sheet";
import { errorText } from "../lib/format";

/** Live or paused, and the switch. Pausing is free; resuming loosens, so it takes a code. */
export function StatusStrip({ overview }: { overview: Overview }) {
  const pause = usePause();
  const [sheet, setSheet] = useState<"pause" | "resume" | null>(null);
  const [code, setCode] = useState("");

  const close = () => {
    setSheet(null);
    setCode("");
    pause.reset();
  };
  const flip = (paused: boolean, given?: string) =>
    pause.mutate({ paused, code: given }, { onSuccess: close });

  return (
    <>
      <section className={overview.paused ? "status paused" : "status"}>
        <div className="status-text">
          <span className={overview.paused ? "dot off" : "dot"} />
          {overview.paused ? "Payments paused" : "Live"}
          <span className="muted">
            {overview.paused
              ? "· nothing is sent until you resume"
              : `· paying within policy v${overview.policy_version}`}
          </span>
        </div>
        <button
          className={overview.paused ? "small primary" : "small ghost"}
          onClick={() =>
            overview.paused && !overview.codes
              ? flip(false)
              : setSheet(overview.paused ? "resume" : "pause")
          }
          disabled={pause.isPending}
        >
          <Icon name={overview.paused ? "play" : "pause"} />
          {overview.paused ? "Resume" : "Pause"}
        </button>
      </section>
      {sheet === "pause" && (
        <Sheet title="Pause payments" onClose={close}>
          <div className="form">
            <p className="muted">
              Nothing is sent until you resume, here or with /resume in Telegram. Approvals already
              waiting stay open.
            </p>
            {pause.error && <p className="error">{errorText(pause.error)}</p>}
            <button className="danger wide" disabled={pause.isPending} onClick={() => flip(true)}>
              <Icon name="pause" />
              Pause all payments
            </button>
          </div>
        </Sheet>
      )}
      {sheet === "resume" && (
        <Sheet title="Resume payments" onClose={close}>
          <form
            className="form"
            onSubmit={(e) => {
              e.preventDefault();
              flip(false, code);
            }}
          >
            <label className="field">
              <span>Authenticator code</span>
              <CodeInput value={code} onChange={setCode} autoFocus invalid={!!pause.error} />
            </label>
            {pause.error && <p className="error">{errorText(pause.error)}</p>}
            <button className="primary wide" disabled={pause.isPending || code.length < 6}>
              Resume
            </button>
          </form>
        </Sheet>
      )}
    </>
  );
}
