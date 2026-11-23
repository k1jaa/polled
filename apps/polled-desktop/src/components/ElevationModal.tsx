interface ElevationModalProps {
  title: string;
  reason: string;
  actionLabel: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ElevationModal({
  title,
  reason,
  actionLabel,
  busy,
  onCancel,
  onConfirm,
}: ElevationModalProps) {
  return (
    <div className="modal-backdrop" role="dialog" aria-modal="true">
      <div className="modal-card">
        <div className="modal-header">
          <span className="badge badge-warn">UAC</span>
          <h2>Administrator permission needed</h2>
        </div>

        <p className="modal-title">{title}</p>
        <p className="modal-reason">{reason}</p>

        <div className="modal-guarantee">
          Polled stays non-elevated and launches <code>polled.exe</code> only
          for this single action. Your original setting is saved to the JSON
          snapshot before any change is applied.
        </div>

        <div className="modal-actions">
          <button
            type="button"
            className="btn btn-secondary"
            disabled={busy}
            onClick={onCancel}
          >
            Cancel
          </button>
          <button
            type="button"
            className="btn btn-admin"
            disabled={busy}
            onClick={onConfirm}
          >
            {busy ? "Waiting for UAC..." : actionLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
