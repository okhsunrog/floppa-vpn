use thiserror::Error;

#[derive(Debug, Error)]
pub enum FloppaError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("User not found: telegram_id={0}")]
    UserNotFound(i64),

    #[error("Peer not found: id={0}")]
    PeerNotFound(i64),

    #[error("Subscription expired")]
    SubscriptionExpired,

    #[error("No active subscription")]
    NoActiveSubscription,

    #[error("Peer limit reached: {current}/{max}")]
    PeerLimitReached { current: i32, max: i32 },

    #[error("No available IPs in subnet")]
    NoAvailableIps,

    #[error("Key generation failed: {0}")]
    KeyGeneration(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("VLESS not configured on this server")]
    VlessNotConfigured,

    #[error("Config error: {0}")]
    Config(#[from] crate::config::ConfigError),
}

pub type Result<T> = std::result::Result<T, FloppaError>;
