//! Per-user Brr service. Execution authority stays on the owning node.
#[cfg(unix)]
pub mod attachment;
#[cfg(all(feature = "host", feature = "fleet", feature = "network"))]
pub mod catalog;
pub mod events;
#[cfg(feature = "network")]
pub mod git_http;
pub mod instance;
pub mod lifecycle;
#[cfg(unix)]
pub mod local;
#[cfg(feature = "host")]
pub mod migration;
#[cfg(feature = "network")]
pub mod network;
pub mod operations;
pub mod paths;
#[cfg(feature = "network")]
pub mod peers;
#[cfg(all(feature = "host", feature = "network"))]
pub mod service;
pub mod settings;
