use anyhow::anyhow;
use parking_lot::RwLock;
use rand::distributions::Alphanumeric;
use rand::Rng;
use tracing::{info, warn};

use ethos_core::types::errors::CoreError;
use ethos_core::types::github::auth::GithubTokens;

use crate::APP_NAME;

/// Keyring entry holding the serialized GithubTokens blob. The credential
/// helper binary reads this same entry, so the name is part of its contract.
pub const GITHUB_TOKENS_KEYRING_USER: &str = "github_oauth_tokens";

/// Refresh the access token once it's within this many minutes of expiry.
/// GitHub user access tokens live 8 hours, so an hour of margin keeps git
/// operations from ever racing an expiring token.
const REFRESH_MARGIN_MINUTES: i64 = 60;

/// An OAuth flow we've opened a browser for and are waiting on the redirect.
/// The Okta token is held so the callback handler can authenticate the code
/// exchange against friendshipper-server.
pub struct PendingOAuthFlow {
    pub state: String,
    pub okta_token: String,
}

#[derive(Default)]
pub struct GithubTokenManager {
    tokens: RwLock<Option<GithubTokens>>,
    pending: RwLock<Option<PendingOAuthFlow>>,
}

impl GithubTokenManager {
    /// Load persisted tokens from the keyring at startup. Expired token sets
    /// with a live refresh token are kept so the refresh loop can revive them.
    pub fn new_from_keyring() -> Self {
        let manager = Self::default();

        match Self::keyring_entry() {
            Ok(entry) => {
                match entry.get_password() {
                    Ok(blob) => match serde_json::from_str::<GithubTokens>(&blob) {
                        Ok(tokens) => {
                            if tokens.can_refresh() {
                                info!("Loaded GitHub tokens from keyring");
                                *manager.tokens.write() = Some(tokens);
                            } else {
                                warn!("Stored GitHub refresh token has expired; reauthorization required");
                            }
                        }
                        Err(e) => warn!("Failed to parse stored GitHub tokens: {}", e),
                    },
                    Err(keyring::Error::NoEntry) => {}
                    Err(e) => warn!("Failed to read GitHub tokens from keyring: {}", e),
                }
            }
            Err(e) => warn!("Failed to open keyring: {}", e),
        }

        manager
    }

    fn keyring_entry() -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(APP_NAME, GITHUB_TOKENS_KEYRING_USER)
    }

    /// Store tokens in memory and persist them to the keyring.
    pub fn set_tokens(&self, tokens: GithubTokens) -> Result<(), CoreError> {
        let blob = serde_json::to_string(&tokens)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to serialize GitHub tokens: {e}")))?;

        Self::keyring_entry()?.set_password(&blob)?;
        *self.tokens.write() = Some(tokens);

        Ok(())
    }

    pub fn clear(&self) -> Result<(), CoreError> {
        *self.tokens.write() = None;
        match Self::keyring_entry()?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn tokens(&self) -> Option<GithubTokens> {
        self.tokens.read().clone()
    }

    /// The current access token, if we have one that hasn't expired.
    pub fn access_token(&self) -> Option<String> {
        self.tokens
            .read()
            .as_ref()
            .filter(|t| !t.is_expired())
            .map(|t| t.access_token.clone())
    }

    /// True when a refresh should be attempted: token inside the refresh
    /// margin (or expired) but the refresh token is still alive.
    pub fn should_refresh(&self) -> bool {
        self.tokens.read().as_ref().is_some_and(|t| {
            t.needs_refresh(chrono::Duration::minutes(REFRESH_MARGIN_MINUTES)) && t.can_refresh()
        })
    }

    /// Begin an OAuth flow: remember the state param and the Okta token that
    /// will authenticate the code exchange. Returns the state string.
    pub fn begin_flow(&self, okta_token: String) -> String {
        let state: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();

        *self.pending.write() = Some(PendingOAuthFlow {
            state: state.clone(),
            okta_token,
        });

        state
    }

    /// Consume the pending flow if the state matches. A mismatch invalidates
    /// the pending flow entirely, since it may indicate a forged callback.
    pub fn take_pending(&self, state: &str) -> Result<PendingOAuthFlow, CoreError> {
        let pending = self.pending.write().take();

        match pending {
            Some(p) if p.state == state => Ok(p),
            Some(_) => Err(CoreError::Internal(anyhow!(
                "GitHub OAuth state mismatch; discarding pending login"
            ))),
            None => Err(CoreError::Internal(anyhow!(
                "No GitHub OAuth flow in progress"
            ))),
        }
    }
}
