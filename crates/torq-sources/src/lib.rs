//! TorQ search sources: declarative JSON/RSS runners (also the plugin
//! mechanism), three bespoke adapters, and result aggregation with dedupe.

pub mod aggregate;
pub mod flat;
pub mod registry;
pub mod rss_src;
pub mod subsplease;
pub mod types;
pub mod util;
pub mod x1337;
pub mod yts;

pub use aggregate::SearchReport;
pub use registry::Registry;
pub use types::{Source, SourceGroup, TorrentResult};

/// Where user plugin TOMLs live: `<config_dir>/torq/plugins/`.
pub fn plugin_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().expect("home dir"))
        .join("torq")
        .join("plugins")
}
