pub mod ini;
pub mod jsonc;
pub mod tomled;
pub mod xml;
pub mod yaml;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error("config could not be parsed: {0}")]
    Unparseable(String),
    #[error("cannot set a key inside a non-object value at '{0}'")]
    NotAnObject(String),
}
