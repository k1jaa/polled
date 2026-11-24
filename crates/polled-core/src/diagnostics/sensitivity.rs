// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Rust competitive FPS sensitivity, eDPI, and `cm/360°` calculator.
//!
//! Converts seamlessly across popular FPS engines (CS2/Source, Valorant, Apex, Overwatch 2,
//! Rainbow Six Siege, Fortnite) and recalculates exact in-game sensitivity when stepping DPI
//! up to 1600/3200 DPI for lower motion-initiation latency.

use serde::{Deserialize, Serialize};

/// Popular competitive FPS games with exact horizontal yaw-per-count constants (degrees).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SensitivityGame {
    #[default]
    Cs2,
    Valorant,
    ApexLegends,
    Overwatch2,
    RainbowSix,
    Fortnite,
}

impl SensitivityGame {
    pub const ALL: [SensitivityGame; 6] = [
        SensitivityGame::Cs2,
        SensitivityGame::Valorant,
        SensitivityGame::ApexLegends,
        SensitivityGame::Overwatch2,
        SensitivityGame::RainbowSix,
        SensitivityGame::Fortnite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SensitivityGame::Cs2 => "CS2 / Quake / Source",
            SensitivityGame::Valorant => "Valorant",
            SensitivityGame::ApexLegends => "Apex Legends",
            SensitivityGame::Overwatch2 => "Overwatch 2 / CoD",
            SensitivityGame::RainbowSix => "Rainbow Six Siege",
            SensitivityGame::Fortnite => "Fortnite (%)",
        }
    }

    /// Degrees of horizontal camera rotation per mouse count at `sensitivity = 1.0`.
    pub fn yaw_deg(self) -> f64 {
        match self {
            SensitivityGame::Cs2 | SensitivityGame::ApexLegends => 0.022,
            SensitivityGame::Valorant => 0.07,
            SensitivityGame::Overwatch2 => 0.0066,
            // Default R6 MouseSensitivityMultiplierUnit = 0.02 -> 0.00572957795 deg/count
            SensitivityGame::RainbowSix => 0.00572957795,
            // Fortnite slider percentage (e.g. 6.4%) -> 0.005555 deg/count per 1%
            SensitivityGame::Fortnite => 0.005555,
        }
    }
}

/// Calculated sensitivity metrics & conversion output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensitivityConversion {
    pub source_edpi: f64,
    pub cm_per_360: f64,
    pub inches_per_360: f64,
    pub target_sens_same_game: f64,
    pub target_sens_cross_game: f64,
}

/// Computes `eDPI`, `cm/360°`, `in/360°`, same-game DPI-step sensitivity, and cross-game sensitivity.
pub fn compute_sensitivity(
    source_game: SensitivityGame,
    source_dpi: f64,
    source_sens: f64,
    target_game: SensitivityGame,
    target_dpi: f64,
) -> Option<SensitivityConversion> {
    if source_dpi <= 0.0
        || source_sens <= 0.0
        || target_dpi <= 0.0
        || !source_dpi.is_finite()
        || !source_sens.is_finite()
        || !target_dpi.is_finite()
    {
        return None;
    }

    let source_edpi = source_dpi * source_sens;
    let counts_per_360 = 360.0 / (source_sens * source_game.yaw_deg());
    let inches_per_360 = counts_per_360 / source_dpi;
    let cm_per_360 = inches_per_360 * 2.54;

    let target_sens_same_game = source_sens * (source_dpi / target_dpi);
    let target_sens_cross_game = 360.0 / (inches_per_360 * target_dpi * target_game.yaw_deg());

    Some(SensitivityConversion {
        source_edpi,
        cm_per_360,
        inches_per_360,
        target_sens_same_game,
        target_sens_cross_game,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_exact_cs2_and_valorant_cm_per_360_and_dpi_step() {
        let cs = compute_sensitivity(
            SensitivityGame::Cs2,
            800.0,
            1.0,
            SensitivityGame::Valorant,
            1600.0,
        )
        .unwrap();
        assert!((cs.source_edpi - 800.0).abs() < 1e-6);
        // 360 / (1.0 * 0.022 * 800) * 2.54 = 51.954545... cm
        assert!((cs.cm_per_360 - 51.9545).abs() < 0.01);
        // Doubling DPI halves in-game sens cleanly.
        assert!((cs.target_sens_same_game - 0.5).abs() < 1e-6);
        // Cross-game Valorant sens at 1600 DPI must yield the exact same cm/360!
        let val_back = compute_sensitivity(
            SensitivityGame::Valorant,
            1600.0,
            cs.target_sens_cross_game,
            SensitivityGame::Cs2,
            800.0,
        )
        .unwrap();
        assert!((val_back.cm_per_360 - cs.cm_per_360).abs() < 0.001);
        assert!((val_back.target_sens_cross_game - 1.0).abs() < 1e-4);
    }

    #[test]
    fn rejects_zero_or_non_finite_inputs() {
        assert!(
            compute_sensitivity(SensitivityGame::Cs2, 0.0, 1.0, SensitivityGame::Cs2, 1600.0)
                .is_none()
        );
        assert!(compute_sensitivity(
            SensitivityGame::Cs2,
            800.0,
            -0.5,
            SensitivityGame::Cs2,
            1600.0
        )
        .is_none());
    }
}
