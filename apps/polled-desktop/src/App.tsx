import { useCallback, useEffect, useMemo, useState } from "react";
import { CheckCard } from "./components/CheckCard";
import { ElevationModal } from "./components/ElevationModal";
import {
  fixAllSafe,
  fixCheck,
  getSnapshotPath,
  scanHealth,
  undoAll,
  undoCheck,
} from "./lib/tauri";
import type { CheckReport, ScanSummary } from "./types";

type FilterMode = "all" | "issues" | "report_only" | "undoable";

interface PendingElevation {
  kind: "fix" | "undo" | "undo_all";
  checkId?: string;
  title: string;
  reason: string;
  actionLabel: string;
}

export function App() {
  const [summary, setSummary] = useState<ScanSummary | null>(null);
  const [snapshotPath, setSnapshotPath] = useState<string>("");
  const [filter, setFilter] = useState<FilterMode>("all");
  const [loading, setLoading] = useState<boolean>(true);
  const [busy, setBusy] = useState<boolean>(false);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [pendingElevation, setPendingElevation] =
    useState<PendingElevation | null>(null);

  const runScan = useCallback(async (showSpinner = true) => {
    if (showSpinner) setLoading(true);
    setErrorMessage(null);
    try {
      const [result, snapPath] = await Promise.all([
        scanHealth(),
        getSnapshotPath(),
      ]);
      setSummary(result);
      setSnapshotPath(snapPath);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void runScan(true);
  }, [runScan]);

  const counts = useMemo(() => {
    if (!summary) {
      return { bad: 0, warn: 0, info: 0, ok: 0, fixableSafe: 0 };
    }
    let bad = 0;
    let warn = 0;
    let info = 0;
    let ok = 0;
    let fixableSafe = 0;
    for (const c of summary.checks) {
      if (c.severity === "bad") bad += 1;
      else if (c.severity === "warn") warn += 1;
      else if (c.severity === "info") info += 1;
      else ok += 1;

      if (
        c.can_fix &&
        !c.requires_elevation &&
        (c.severity === "warn" || c.severity === "bad")
      ) {
        fixableSafe += 1;
      }
    }
    return { bad, warn, info, ok, fixableSafe };
  }, [summary]);

  const undoableSet = useMemo(
    () => new Set(summary?.undoable_check_ids ?? []),
    [summary]
  );

  const visibleChecks = useMemo(() => {
    if (!summary) return [];
    return summary.checks.filter((c) => {
      if (filter === "issues") return c.severity === "bad" || c.severity === "warn";
      if (filter === "report_only") return !c.can_fix;
      if (filter === "undoable") return undoableSet.has(c.id);
      return true;
    });
  }, [summary, filter, undoableSet]);

  async function handleFixClick(check: CheckReport) {
    setErrorMessage(null);
    setStatusMessage(null);

    if (check.requires_elevation) {
      setPendingElevation({
        kind: "fix",
        checkId: check.id,
        title: `Fix "${check.name}"`,
        reason:
          check.elevation_reason ??
          "Windows requires administrator permissions to update this system setting.",
        actionLabel: "Continue with UAC",
      });
      return;
    }

    setBusy(true);
    try {
      await fixCheck(check.id, false);
      setStatusMessage(`Fixed "${check.name}". Original value saved to undo snapshot.`);
      await runScan(false);
    } catch (err) {
      const msg = String(err);
      if (msg.startsWith("ELEVATION_REQUIRED:")) {
        setPendingElevation({
          kind: "fix",
          checkId: check.id,
          title: `Fix "${check.name}"`,
          reason: msg.replace("ELEVATION_REQUIRED:", ""),
          actionLabel: "Continue with UAC",
        });
      } else {
        setErrorMessage(msg);
      }
    } finally {
      setBusy(false);
    }
  }

  async function handleUndoClick(check: CheckReport) {
    setErrorMessage(null);
    setStatusMessage(null);

    if (check.requires_elevation) {
      setPendingElevation({
        kind: "undo",
        checkId: check.id,
        title: `Undo "${check.name}"`,
        reason:
          check.elevation_reason ??
          "Restoring this system setting requires administrator permissions.",
        actionLabel: "Restore with UAC",
      });
      return;
    }

    setBusy(true);
    try {
      const updated = await undoCheck(check.id, false);
      setSummary(updated);
      setStatusMessage(`Restored "${check.name}" from baseline snapshot.`);
    } catch (err) {
      const msg = String(err);
      if (msg.startsWith("ELEVATION_REQUIRED:")) {
        setPendingElevation({
          kind: "undo",
          checkId: check.id,
          title: `Undo "${check.name}"`,
          reason: msg.replace("ELEVATION_REQUIRED:", ""),
          actionLabel: "Restore with UAC",
        });
      } else {
        setErrorMessage(msg);
      }
    } finally {
      setBusy(false);
    }
  }

  async function handleFixAllSafe() {
    setBusy(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const updated = await fixAllSafe();
      setSummary(updated);
      setStatusMessage(
        "Applied all safe user-level fixes. Original values saved to undo snapshot."
      );
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function handleUndoAll() {
    setBusy(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const result = await undoAll(false);
      await runScan(false);
      const needsAdmin = result.failed.some((f) => f.requires_elevation);
      if (needsAdmin) {
        setPendingElevation({
          kind: "undo_all",
          title: "Restore remaining system-level settings",
          reason:
            "One or more settings in your undo snapshot require administrator privileges to restore.",
          actionLabel: "Restore remaining with UAC",
        });
      } else if (result.failed.length > 0) {
        setErrorMessage(
          `Restored ${result.restored_ids.length} setting(s), but ${result.failed.length} failed.`
        );
      } else {
        setStatusMessage(
          `Restored all ${result.restored_ids.length} original setting(s) from your baseline snapshot.`
        );
      }
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function confirmElevation() {
    if (!pendingElevation) return;
    setBusy(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      if (pendingElevation.kind === "fix" && pendingElevation.checkId) {
        await fixCheck(pendingElevation.checkId, true);
        setStatusMessage(`Applied elevated fix for "${pendingElevation.checkId}".`);
      } else if (pendingElevation.kind === "undo" && pendingElevation.checkId) {
        await undoCheck(pendingElevation.checkId, true);
        setStatusMessage(`Restored "${pendingElevation.checkId}" with administrator approval.`);
      } else if (pendingElevation.kind === "undo_all") {
        await undoAll(true);
        setStatusMessage("Restored remaining settings from baseline snapshot.");
      }
      setPendingElevation(null);
      await runScan(false);
    } catch (err) {
      setErrorMessage(String(err));
      setPendingElevation(null);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand">
          <div className="brand-dot" aria-hidden="true" />
          <div>
            <h1>Polled</h1>
            <p className="subtitle">
              Input Settings Health Check
              {summary && (
                <span className="win-tag">Win {summary.windows_version}</span>
              )}
              {summary?.windows_build_changed && (
                <span className="win-tag win-tag-warn">Windows Updated</span>
              )}
            </p>
          </div>
        </div>

        <div className="topbar-actions">
          <button
            type="button"
            className="btn btn-secondary"
            disabled={busy || loading}
            onClick={() => void runScan(true)}
          >
            Rescan
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || loading || counts.fixableSafe === 0}
            onClick={() => void handleFixAllSafe()}
          >
            Fix Safe Issues ({counts.fixableSafe})
          </button>
          <button
            type="button"
            className="btn btn-danger-outline"
            disabled={busy || loading || !summary?.has_undo_snapshot}
            onClick={() => void handleUndoAll()}
          >
            Undo All ({summary?.undoable_check_ids.length ?? 0})
          </button>
        </div>
      </header>

      <main className="main-content">
        {summary && summary.regressions.length > 0 && (
          <section className="banner banner-warn">
            <strong>
              {summary.windows_build_changed
                ? "Windows Update changed your input settings:"
                : "Regressions detected since your previous scan:"}
            </strong>
            <ul>
              {summary.regressions.map((reg) => (
                <li key={reg.check_id}>
                  <strong>{reg.name}</strong>: {reg.previous.toUpperCase()} →{" "}
                  {reg.current.toUpperCase()} ({reg.current_value})
                </li>
              ))}
            </ul>
          </section>
        )}

        {statusMessage && (
          <div className="banner banner-ok">{statusMessage}</div>
        )}
        {errorMessage && (
          <div className="banner banner-bad">{errorMessage}</div>
        )}

        <section className="toolbar">
          <div className="filter-tabs" role="tablist">
            <button
              type="button"
              className={`tab ${filter === "all" ? "tab-active" : ""}`}
              onClick={() => setFilter("all")}
            >
              All ({summary?.checks.length ?? 0})
            </button>
            <button
              type="button"
              className={`tab ${filter === "issues" ? "tab-active" : ""}`}
              onClick={() => setFilter("issues")}
            >
              Issues ({counts.bad + counts.warn})
            </button>
            <button
              type="button"
              className={`tab ${filter === "report_only" ? "tab-active" : ""}`}
              onClick={() => setFilter("report_only")}
            >
              Report Only
            </button>
            <button
              type="button"
              className={`tab ${filter === "undoable" ? "tab-active" : ""}`}
              onClick={() => setFilter("undoable")}
            >
              Undoable ({summary?.undoable_check_ids.length ?? 0})
            </button>
          </div>

          <div className="summary-pills">
            <span className="pill-ok">{counts.ok} OK</span>
            <span className="pill-info">{counts.info} Info</span>
            <span className="pill-warn">{counts.warn} Warn</span>
            <span className="pill-bad">{counts.bad} Bad</span>
          </div>
        </section>

        {loading ? (
          <div className="empty-state">Scanning Windows input settings...</div>
        ) : visibleChecks.length === 0 ? (
          <div className="empty-state">
            No checks match the selected filter.
          </div>
        ) : (
          <section className="checks-grid">
            {visibleChecks.map((check) => (
              <CheckCard
                key={check.id}
                check={check}
                isUndoable={undoableSet.has(check.id)}
                busy={busy}
                onFix={(c) => void handleFixClick(c)}
                onUndo={(c) => void handleUndoClick(c)}
              />
            ))}
          </section>
        )}
      </main>

      <footer className="footer">
        <span className="footer-path" title={snapshotPath}>
          Snapshot: <code>{snapshotPath || "%LOCALAPPDATA%\\Polled\\baseline_snapshot.json"}</code>
        </span>
        <span className="footer-meta">
          Close window to minimize to tray (low RAM) • No telemetry • GPL-3.0
        </span>
      </footer>

      {pendingElevation && (
        <ElevationModal
          title={pendingElevation.title}
          reason={pendingElevation.reason}
          actionLabel={pendingElevation.actionLabel}
          busy={busy}
          onCancel={() => setPendingElevation(null)}
          onConfirm={() => void confirmElevation()}
        />
      )}
    </div>
  );
}

export default App;
