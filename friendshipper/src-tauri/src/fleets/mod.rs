pub use router::router;

pub mod router;

use anyhow::anyhow;
use ethos_core::types::errors::CoreError;

/// The two ways to promote a build, chosen by the `promotionViaCoordinator` dynamic config switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromotePath {
    /// Create the promote workflow directly, targets from `promotableBuildShards`.
    Workflow,
    /// Create a `Promotion` for the fleet coordinator, targets from `ShardConfig`.
    Coordinator,
}

impl PromotePath {
    pub fn from_switch(via_coordinator: bool) -> Self {
        if via_coordinator {
            PromotePath::Coordinator
        } else {
            PromotePath::Workflow
        }
    }

    /// Refuses a request on this path when the switch selects the other one, so a stale UI can't
    /// promote around the coordinator, or through it before it is turned on.
    pub fn require(self, via_coordinator: bool) -> Result<(), CoreError> {
        let active = Self::from_switch(via_coordinator);
        if self == active {
            return Ok(());
        }
        Err(CoreError::Input(anyhow!(match active {
            PromotePath::Coordinator =>
                "Promotions go through the fleet coordinator now; reload Friendshipper and promote from the shard picker",
            PromotePath::Workflow =>
                "Promotion through the fleet coordinator is turned off (promotionViaCoordinator)",
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_picks_the_path() {
        assert_eq!(PromotePath::from_switch(false), PromotePath::Workflow);
        assert_eq!(PromotePath::from_switch(true), PromotePath::Coordinator);
    }

    #[test]
    fn only_the_active_path_is_allowed() {
        assert!(PromotePath::Workflow.require(false).is_ok());
        assert!(PromotePath::Coordinator.require(true).is_ok());
        assert!(matches!(
            PromotePath::Workflow.require(true),
            Err(CoreError::Input(_))
        ));
        assert!(matches!(
            PromotePath::Coordinator.require(false),
            Err(CoreError::Input(_))
        ));
    }
}
