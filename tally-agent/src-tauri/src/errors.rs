use thiserror::Error;

#[derive(Debug, Error)]
pub enum TallyError {
    #[error("Tally endpoint must be loopback (127.0.0.1); got {0}")]
    NonLoopbackHost(String),

    #[error("Tally connection refused at {0}")]
    ConnectionRefused(String),

    #[error("Tally request timed out after {0}s")]
    Timeout(u64),

    #[error("Malformed Tally XML response: {0}")]
    MalformedXml(String),

    #[error("Tally HTTP error: status {0}")]
    HttpError(u16),

    #[error("Tally transport error: {0}")]
    Transport(String),

    #[error("No active company in Tally")]
    NoActiveCompany,

    #[error("Missing ALTERID in Tally response")]
    MissingAlterId,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Agent is not paired")]
    NotPaired,

    #[error("Pairing failed")]
    PairingFailed,

    #[error("Sync push failed with status {0}")]
    SyncFailed(u16),

    #[error("Heartbeat failed with status {0}")]
    HeartbeatFailed(u16),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Invalid API response: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Credential vault error: {0}")]
    Keyring(String),

    #[error("No agent token stored")]
    NotFound,
}

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("Failed to read checkpoint: {0}")]
    Read(String),

    #[error("Failed to write checkpoint: {0}")]
    Write(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Tally(#[from] TallyError),

    #[error(transparent)]
    Api(#[from] ApiError),

    #[error(transparent)]
    Vault(#[from] VaultError),

    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),

    #[error("Sync already in progress")]
    SyncInProgress,

    #[error("Initial backfill required before delta sync")]
    BackfillRequired,

    #[error("{0}")]
    Other(String),
}
