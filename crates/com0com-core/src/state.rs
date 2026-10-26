//! State snapshots, persistence options and restore reports.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{KernelKind, PairId, PairParams, SdkError, MAX_PAIR_ID};

/// 快照格式版本。
///
/// 2：噪声概率从 0.01% 单位改为上游的 1/100_000_000 单位，并新增
/// `all_data_bits`。加载 schema 1 文件时会自动换算（见 `state_io::load`）。
pub const SNAPSHOT_SCHEMA: u32 = 2;

/// Portable description of all pairs. PTY paths and handles are deliberately
/// not stored: they are recreated on restore.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub created_at: SystemTime,
    pub platform: String,
    pub kernel: String,
    pub pairs: Vec<SnapshotPair>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPair {
    pub id: PairId,
    pub params: PairParams,
}

impl Snapshot {
    pub fn new(platform: impl Into<String>, kernel: KernelKind, pairs: Vec<SnapshotPair>) -> Self {
        Self {
            schema: SNAPSHOT_SCHEMA,
            created_at: SystemTime::now(),
            platform: platform.into(),
            kernel: kernel.as_str().to_string(),
            pairs,
        }
    }

    pub fn validate(&self) -> Result<(), SdkError> {
        if self.schema != SNAPSHOT_SCHEMA {
            return Err(SdkError::Protocol(format!(
                "unsupported snapshot schema {} (expected {SNAPSHOT_SCHEMA})",
                self.schema
            )));
        }
        for pair in &self.pairs {
            if pair.id > MAX_PAIR_ID {
                return Err(SdkError::Protocol(format!(
                    "snapshot pair id {} is out of range",
                    pair.id
                )));
            }
            pair.params.validate()?;
        }
        Ok(())
    }

    pub fn pair(&self, id: PairId) -> Option<&SnapshotPair> {
        self.pairs.iter().find(|pair| pair.id == id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOptions {
    pub dry_run: bool,
    pub remove_extra: bool,
    pub continue_on_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFailure {
    pub pair: PairId,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub dry_run: bool,
    pub applied: Vec<PairId>,
    pub failed: Vec<RestoreFailure>,
}

impl RestoreReport {
    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip_via_json() {
        let snapshot = Snapshot::new(
            "windows",
            KernelKind::WindowsKmdfWdm,
            vec![SnapshotPair {
                id: 3,
                params: PairParams::default(),
            }],
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, decoded);
        assert!(decoded.validate().is_ok());
        assert!(decoded.pair(3).is_some());
        assert!(decoded.pair(4).is_none());
    }

    #[test]
    fn rejects_wrong_schema_and_bad_ids() {
        let mut snapshot = Snapshot::new("linux", KernelKind::UnixPty, Vec::new());
        snapshot.schema = 99;
        assert!(matches!(snapshot.validate(), Err(SdkError::Protocol(_))));

        let mut snapshot = Snapshot::new("linux", KernelKind::UnixPty, Vec::new());
        snapshot.pairs.push(SnapshotPair {
            id: 64,
            params: PairParams::default(),
        });
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn report_success_semantics() {
        let mut report = RestoreReport::default();
        assert!(report.is_success());
        report.failed.push(RestoreFailure {
            pair: 1,
            error: "boom".into(),
        });
        assert!(!report.is_success());
    }
}
