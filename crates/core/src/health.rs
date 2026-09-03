//! Health thresholds and classification — the Rust mirror of
//! `@yukinal/shared` (`types/health.ts`).
//!
//! The canonical numbers live in `packages/shared/fixtures/health_thresholds.json`;
//! this module compiles that file in (`include_str!`) and parses it once at first
//! use. A unit test pins the parsed values, so UI (TS) and core (Rust) classify
//! usage exactly the same way, off one file.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthClass {
    Healthy,
    Warning,
    Critical,
}

impl HealthClass {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct UsageThresholds {
    pub warning: f64,
    pub critical: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct HealthThresholds {
    pub cpu: UsageThresholds,
    pub memory: UsageThresholds,
    pub disk: UsageThresholds,
}

const FIXTURE: &str = include_str!("../../../packages/shared/fixtures/health_thresholds.json");

/// 用共享夹具解析出的阈值（与 shared 同一份数值）。
pub static THRESHOLDS: LazyLock<HealthThresholds> = LazyLock::new(|| {
    serde_json::from_str(FIXTURE)
        .expect("shared health_thresholds.json fixture must stay valid JSON")
});

/// usage → class（与 shared 的 `healthClass` 同算术）。
#[must_use]
pub fn classify(usage_percent: f64, thresholds: UsageThresholds) -> HealthClass {
    if usage_percent >= thresholds.critical {
        HealthClass::Critical
    } else if usage_percent >= thresholds.warning {
        HealthClass::Warning
    } else {
        HealthClass::Healthy
    }
}

/// 整体快照健康度：拿各自阈值判 cpu/mem/disk，取最差；全缺返回 None。
#[must_use]
pub fn overall(cpu: Option<f64>, memory: Option<f64>, disk: Option<f64>) -> Option<HealthClass> {
    use HealthClass as H;
    let mut seen = false;
    let mut worst = H::Healthy;
    for (value, thresholds) in [
        (cpu, THRESHOLDS.cpu),
        (memory, THRESHOLDS.memory),
        (disk, THRESHOLDS.disk),
    ] {
        if let Some(value) = value {
            seen = true;
            let class = classify(value, thresholds);
            if class == H::Critical {
                return Some(H::Critical);
            }
            if class == H::Warning {
                worst = H::Warning;
            }
        }
    }
    seen.then_some(worst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_pin_to_the_shared_fixture() {
        let fixture: HealthThresholds = serde_json::from_str(FIXTURE).expect("parse fixture");
        assert_eq!(fixture.cpu.warning, THRESHOLDS.cpu.warning);
        assert_eq!(fixture.cpu.critical, THRESHOLDS.cpu.critical);
        assert_eq!(fixture.memory.warning, THRESHOLDS.memory.warning);
        assert_eq!(fixture.memory.critical, THRESHOLDS.memory.critical);
        assert_eq!(fixture.disk.warning, THRESHOLDS.disk.warning);
        assert_eq!(fixture.disk.critical, THRESHOLDS.disk.critical);
    }

    #[test]
    fn classifies_usage_at_the_thresholds() {
        assert_eq!(classify(0.0, THRESHOLDS.cpu), HealthClass::Healthy);
        assert_eq!(classify(69.9, THRESHOLDS.cpu), HealthClass::Healthy);
        assert_eq!(classify(70.0, THRESHOLDS.cpu), HealthClass::Warning);
        assert_eq!(classify(89.0, THRESHOLDS.cpu), HealthClass::Warning);
        assert_eq!(classify(90.0, THRESHOLDS.cpu), HealthClass::Critical);
        assert_eq!(classify(100.0, THRESHOLDS.cpu), HealthClass::Critical);
    }

    #[test]
    fn overall_takes_the_worst_signal() {
        assert_eq!(
            overall(Some(40.0), Some(50.0), Some(55.0)),
            Some(HealthClass::Healthy)
        );
        assert_eq!(
            overall(Some(80.0), Some(10.0), Some(10.0)),
            Some(HealthClass::Warning)
        );
        assert_eq!(
            overall(Some(95.0), Some(1.0), None),
            Some(HealthClass::Critical)
        );
        assert_eq!(overall(None, None, None), None);
    }
}
