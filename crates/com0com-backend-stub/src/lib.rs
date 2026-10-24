//! An explicit backend for unsupported platforms.
//!
//! Every operation returns [`SdkError::Unsupported`] and the capability set is
//! empty, so UI code that checks capabilities hides all functionality.

use com0com_core::{
    Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, EventFilter, EventSubscription,
    KernelKind, LogEntry, MonitorHandle, Pair, PairId, PairParams, PortConfig, PortInfo, PortName,
    SdkError,
};

pub struct StubBackend {
    info: BackendInfo,
}

impl StubBackend {
    pub fn new(platform: impl Into<String>) -> Self {
        Self {
            info: BackendInfo::new(platform, KernelKind::None, None),
        }
    }
}

impl Backend for StubBackend {
    fn info(&self) -> BackendInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE
    }

    fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn create_pair(&self, _id: Option<PairId>, _params: &PairParams) -> Result<Pair, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn delete_pair(&self, _id: PairId) -> Result<(), SdkError> {
        Err(SdkError::Unsupported)
    }

    fn get_params(&self, _id: PairId) -> Result<PairParams, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn set_params(&self, _id: PairId, _params: &PairParams) -> Result<(), SdkError> {
        Err(SdkError::Unsupported)
    }

    fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn open_monitor(
        &self,
        _port: &PortName,
        _cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn driver_info(&self, _kind: DriverKind) -> Result<DriverInfo, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn driver_logs(&self, _limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        Err(SdkError::Unsupported)
    }

    fn subscribe(&self, _filter: EventFilter) -> Result<EventSubscription, SdkError> {
        Err(SdkError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_nothing_and_fails_explicitly() {
        let backend = StubBackend::new("plan9");
        assert_eq!(backend.info().platform, "plan9");
        assert_eq!(backend.capabilities(), Capabilities::NONE);
        assert_eq!(backend.list_pairs(), Err(SdkError::Unsupported));
        assert_eq!(backend.delete_pair(0), Err(SdkError::Unsupported));
    }
}
