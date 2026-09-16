#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCredentialCleanup {
    pub reference: String,
    pub created_at: String,
    pub attempts: i64,
    pub last_error: String,
}
