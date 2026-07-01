use thiserror::Error;

pub type Result<T> = std::result::Result<T, CommonError>;

#[derive(Debug, Error)]
pub enum CommonError {
    #[error("invalid subdomain: {0}")]
    InvalidSubdomain(String),
    #[error("reserved subdomain: {0}")]
    ReservedSubdomain(String),
    #[error("password hash error: {0}")]
    PasswordHash(String),
    #[error("config error: {0}")]
    Config(String),
}
