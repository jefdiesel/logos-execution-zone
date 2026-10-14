use borsh::{BorshDeserialize, BorshSerialize};
use lee::{AccountId, PublicAccountEvidence};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum DepositRecipient {
    Identified(PublicAccountEvidence) = 1,
}

impl DepositRecipient {
    #[must_use]
    pub fn account_id(&self) -> AccountId {
        self.evidence().account_id()
    }

    #[must_use]
    pub const fn evidence(&self) -> &PublicAccountEvidence {
        let Self::Identified(evidence) = self;
        evidence
    }
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct DepositMetadata {
    pub recipient: DepositRecipient,
}
