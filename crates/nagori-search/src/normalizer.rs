// Canonical normalization lives in `nagori_core::text`. This module re-exports
// it so existing call sites keep compiling without bouncing between crates.
pub use nagori_core::normalize_text;
