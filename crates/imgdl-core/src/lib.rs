mod config;
mod dns;
mod orchestrator;
mod output;
mod pool;
mod retry;
mod tls;
mod transport;
mod types;

pub use config::{Config, NamingStrategy};
pub use orchestrator::Downloader;
pub use types::{DownloadError, DownloadOutcome, DownloadResult};
