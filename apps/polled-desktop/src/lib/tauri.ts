import { invoke } from "@tauri-apps/api/core";
import type { CheckReport, ScanSummary, UndoSummary } from "../types";

export async function scanHealth(): Promise<ScanSummary> {
  return invoke<ScanSummary>("scan_health");
}

export async function getSnapshotPath(): Promise<string> {
  return invoke<string>("get_snapshot_path");
}

export async function fixCheck(
  checkId: string,
  elevated: boolean
): Promise<CheckReport> {
  return invoke<CheckReport>("fix_check", { checkId, elevated });
}

export async function fixAllSafe(): Promise<ScanSummary> {
  return invoke<ScanSummary>("fix_all_safe");
}

export async function undoCheck(
  checkId: string,
  elevated: boolean
): Promise<ScanSummary> {
  return invoke<ScanSummary>("undo_check", { checkId, elevated });
}

export async function undoAll(elevated: boolean): Promise<UndoSummary> {
  return invoke<UndoSummary>("undo_all", { elevated });
}
