import type { CheckReport } from "../types";

interface CheckCardProps {
  check: CheckReport;
  isUndoable: boolean;
  busy: boolean;
  onFix: (check: CheckReport) => void;
  onUndo: (check: CheckReport) => void;
}

const SEVERITY_LABEL: Record<CheckReport["severity"], string> = {
  ok: "OK",
  info: "INFO",
  warn: "WARN",
  bad: "BAD",
};

export function CheckCard({
  check,
  isUndoable,
  busy,
  onFix,
  onUndo,
}: CheckCardProps) {
  const showFixButton =
    check.can_fix &&
    (check.severity === "bad" ||
      check.severity === "warn" ||
      (check.id === "power_plan" && check.severity === "info"));

  return (
    <article className={`check-card severity-border-${check.severity}`}>
      <div className="check-header">
        <div className="check-title-group">
          <span className={`badge badge-${check.severity}`}>
            {SEVERITY_LABEL[check.severity]}
          </span>
          <div>
            <h3 className="check-name">{check.name}</h3>
            <code className="check-id">{check.id}</code>
          </div>
        </div>

        <div className="check-actions">
          {!check.can_fix && <span className="pill-muted">Report only</span>}
          {check.requires_relogin && check.severity !== "ok" && (
            <span className="pill-note" title="Windows reads this value at sign-in">
              Sign-out required
            </span>
          )}
          {isUndoable && (
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={busy}
              onClick={() => onUndo(check)}
            >
              Undo
            </button>
          )}
          {showFixButton && (
            <button
              type="button"
              className={`btn btn-sm ${
                check.requires_elevation ? "btn-admin" : "btn-primary"
              }`}
              disabled={busy}
              onClick={() => onFix(check)}
            >
              {check.requires_elevation ? "Fix (Admin)" : "Fix"}
            </button>
          )}
        </div>
      </div>

      <div className="check-values">
        <div className="value-box">
          <span className="value-label">Current</span>
          <span className="value-text">{check.current_value}</span>
        </div>
        <div className="value-box">
          <span className="value-label">Recommended</span>
          <span className="value-text value-recommended">
            {check.recommended_value}
          </span>
        </div>
      </div>

      <p className="check-reason">{check.reason}</p>
    </article>
  );
}
