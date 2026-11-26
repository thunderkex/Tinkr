pub mod adapters;
pub mod auto;
pub mod compress;
pub mod defaults;
pub mod edit;
pub mod filters;
pub mod global_scope;
pub mod hook;
pub mod ledger;
pub mod lifecycle;
pub mod manifest;
pub mod mcp;
pub mod outliers;
pub mod overlap;
pub mod platform;
pub mod profile;
pub mod provider;
pub mod read_dedup;
pub mod recall;
pub mod remote;
pub mod runner;
pub mod say_less;
pub mod scope;
pub mod tokenizer;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot determine home/config directories")]
    NoHome,
    #[error("recall id '{0}' not found")]
    RecallNotFound(String),
    #[error("recall id '{0}' is ambiguous")]
    RecallAmbiguous(String),
    #[error("invalid recall id '{0}'")]
    RecallInvalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
