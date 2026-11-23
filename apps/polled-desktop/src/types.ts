export type Severity = "ok" | "info" | "warn" | "bad";

export interface CheckReport {
  id: string;
  name: string;
  current_value: string;
  recommended_value: string;
  reason: string;
  severity: Severity;
  can_fix: boolean;
  requires_elevation: boolean;
  elevation_reason: string | null;
  requires_relogin: boolean;
}

export interface Regression {
  check_id: string;
  name: string;
  previous: Severity;
  current: Severity;
  current_value: string;
  reason: string;
}

export interface ScanSummary {
  scanned_at: string;
  windows_version: string;
  windows_build_changed: boolean;
  previous_windows_version: string | null;
  has_undo_snapshot: boolean;
  undoable_check_ids: string[];
  checks: CheckReport[];
  regressions: Regression[];
}

export interface UndoFailure {
  check_id: string;
  error: string;
  requires_elevation: boolean;
}

export interface UndoSummary {
  restored_ids: string[];
  failed: UndoFailure[];
  remaining_in_snapshot: number;
}
