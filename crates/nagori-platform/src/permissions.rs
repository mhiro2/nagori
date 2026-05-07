use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    Accessibility,
    InputMonitoring,
    Clipboard,
    Notifications,
    AutoLaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionState {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionStatus {
    pub kind: PermissionKind,
    pub state: PermissionState,
    pub message: Option<String>,
}

#[async_trait]
pub trait PermissionChecker: Send + Sync {
    async fn check(&self) -> Result<Vec<PermissionStatus>>;
    async fn request(&self, permission: PermissionKind) -> Result<PermissionStatus>;
}
