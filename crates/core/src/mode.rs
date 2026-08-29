use serde::{Deserialize, Serialize};

/// Operating mode. Ordered from least to most autonomous.
///
/// * `Observe`  – read-only. Proposals are recorded, never executed.
/// * `Assist`   – proposals execute only after per-transaction approval.
/// * `Automate` – tier-0 allow-listed rules execute without approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Observe,
    Assist,
    Automate,
}

/// Risk tier of a rule. Only `Tier0` rules may run in `Automate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskTier {
    /// Reversible, never touches project folders, never trashes.
    Tier0,
    /// Reversible but touches user-organised locations.
    Tier1,
    /// Trashes or archives.
    Tier2,
}

impl RiskTier {
    pub fn from_u8(t: u8) -> Self {
        match t {
            0 => RiskTier::Tier0,
            1 => RiskTier::Tier1,
            _ => RiskTier::Tier2,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            RiskTier::Tier0 => 0,
            RiskTier::Tier1 => 1,
            RiskTier::Tier2 => 2,
        }
    }
}

impl Mode {
    /// Whether a rule of the given tier may execute without explicit approval.
    pub fn allows_unattended(self, tier: RiskTier) -> bool {
        matches!((self, tier), (Mode::Automate, RiskTier::Tier0))
    }

    /// Whether any mutation at all is possible in this mode.
    pub fn can_mutate(self) -> bool {
        self != Mode::Observe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_never_mutates() {
        assert!(!Mode::Observe.can_mutate());
        assert!(!Mode::Observe.allows_unattended(RiskTier::Tier0));
    }

    #[test]
    fn only_tier0_runs_unattended() {
        assert!(Mode::Automate.allows_unattended(RiskTier::Tier0));
        assert!(!Mode::Automate.allows_unattended(RiskTier::Tier1));
        assert!(!Mode::Assist.allows_unattended(RiskTier::Tier0));
    }
}
