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

    #[error("Installation does not belong to this user: id={0}")]
    InvalidInstallation(i64),

    #[error("An active {protocol} peer already exists for installation {installation_id}")]
    PeerAlreadyExists {
        installation_id: i64,
        protocol: &'static str,
    },

    #[error("No available IPs in subnet")]
    NoAvailableIps,

    #[error("WireGuard key error: {0}")]
    Key(#[from] crate::wg_keys::KeyError),

    #[error("Encryption error: {0}")]
    Crypto(#[from] crate::crypto::CryptoError),

    #[error("Password hashing error: {0}")]
    PasswordHash(#[from] argon2::password_hash::Error),

    #[error("Blocking task failed: {0}")]
    BlockingTask(#[from] tokio::task::JoinError),

    #[error("Stored UUID is malformed: {0}")]
    MalformedUuid(#[from] uuid::Error),

    #[error("VLESS not configured on this server")]
    VlessNotConfigured,

    #[error("AmneziaWG not configured on this server")]
    AmneziaWgNotConfigured,

    #[error("Login already taken")]
    CredentialTaken,

    #[error("Invalid login or password")]
    InvalidCredentials,

    #[error("Invalid login: {0}")]
    InvalidLogin(String),

    #[error("Invalid password: {0}")]
    InvalidPassword(#[from] crate::password::PasswordError),

    #[error("Config error: {0}")]
    Config(#[from] crate::config::ConfigError),
}

pub type Result<T> = std::result::Result<T, FloppaError>;
