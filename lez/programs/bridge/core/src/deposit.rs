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

#[cfg(test)]
mod tests {
    use lee::{PrivateKey, PublicKey};
    use lee_core::program::PdaSeed;

    use super::*;

    fn key() -> PublicKey {
        PublicKey::new_from_private_key(&PrivateKey::try_new([3; 32]).unwrap())
    }

    #[test]
    fn deposit_metadata_carries_key_or_pda_evidence_under_tag_one() {
        let key = key();
        let pda = PublicAccountEvidence::Pda {
            program: AccountId::new([4; 32]),
            seed: PdaSeed::new([6; 32]),
        };
        for (evidence, bytes) in [
            (
                PublicAccountEvidence::Key(key.clone()),
                [&[1, 0][..], &key.value()[..]].concat(),
            ),
            (pda, [&[1, 1][..], &[4; 32][..], &[6; 32][..]].concat()),
        ] {
            let recipient = DepositRecipient::Identified(evidence);
            let metadata = DepositMetadata {
                recipient: recipient.clone(),
            };
            assert_eq!(borsh::to_vec(&metadata).unwrap(), bytes);
            assert_eq!(
                DepositMetadata::try_from_slice(&bytes).unwrap().recipient,
                recipient
            );
        }
    }

    #[test]
    fn deposit_metadata_rejects_the_address_form_unknown_tags_and_misfit_lengths() {
        let key = key();
        for bytes in [
            [&[0][..], &key.value()[..]].concat(),
            [&[2, 0][..], &key.value()[..]].concat(),
            [&[1, 0][..], &key.value()[..31]].concat(),
            [&[1, 0][..], &key.value()[..], &[0][..]].concat(),
        ] {
            assert!(
                DepositMetadata::try_from_slice(&bytes).is_err(),
                "{bytes:?}"
            );
        }
    }
}
