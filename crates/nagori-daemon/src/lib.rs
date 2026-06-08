pub mod ai_registry;
pub mod capture_loop;
pub mod health;
pub mod ipc_handler;
pub mod maintenance;
pub mod runtime;
pub mod search_cache;
pub mod semantic_index;
pub mod serve;
pub mod thumbnails;

pub use capture_loop::CaptureLoop;
pub use health::{
    CAPTURE_DEGRADED_THRESHOLD, CaptureHealth, MAINTENANCE_DEGRADED_THRESHOLD, MaintenanceHealth,
    StartupHealth,
};
pub use maintenance::{MaintenanceReport, MaintenanceService};
pub use runtime::{NagoriRuntime, NagoriRuntimeBuilder, ShutdownHandle};
pub use search_cache::{
    CACHEABLE_QUERY_LEN, CacheKey, CacheLookup, DEFAULT_CACHE_CAPACITY, RecentSearchCache,
    SharedSearchCache, new_shared_cache,
};
pub use serve::{DaemonConfig, acquire_data_dir_lock, default_socket_path, run_daemon};
