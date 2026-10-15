//! `system_upgrader`: upgrades the system programs, each one of its PDAs
//! (`PdaSeed::for_system_program(name)`). Pending upgrades live in its registry. Only the block
//! producer includes its transactions.

use std::collections::BTreeMap;

use borsh::{BorshDeserialize, BorshSerialize};
pub use lee_core::program::{SYSTEM_UPGRADER_ACCOUNT_ID, SystemProgramName};
use lee_core::{
    BlockId,
    account::{AccountId, Actor},
    program::{PROGRAM_LOADER_ACCOUNT_ID, PdaSeed},
};

const REGISTRY_SEED: [u8; 32] = *b"/LEZ/v0.3/SystemRegistry/0000000";

/// Sent at the top level to the registry's actor ([`registry_actor`]). Variants are append-only.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    /// Schedules `name` to switch to the chain at `first_segment` from block `from_height`.
    Schedule {
        name: SystemProgramName,
        first_segment: AccountId,
        from_height: BlockId,
    },
    /// Applies `name`'s pending upgrade, from its height, and clears it. Declares the system
    /// program's loader actor ([`loader_actor`]), which it sends the new chain to.
    Apply {
        name: SystemProgramName,
        from_height: BlockId,
    },
    /// Clears `name`'s pending upgrade, at any height, if it is exactly this one.
    Cancel {
        name: SystemProgramName,
        first_segment: AccountId,
        from_height: BlockId,
    },
}

/// A pending upgrade, stored in the [`Registry`] under the system program's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ScheduledUpgrade {
    pub first_segment: AccountId,
    pub from_height: BlockId,
}

/// Every upgradable system program, by name, with its pending upgrade, if any. Held in the
/// registry's actor state ([`registry_actor`]): seeded at genesis.
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Registry {
    pub programs: BTreeMap<SystemProgramName, Option<ScheduledUpgrade>>,
}

impl Registry {
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("a registry serializes")
    }

    /// An empty registry for an empty actor state.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return Some(Self::default());
        }
        borsh::from_slice(bytes).ok()
    }

    /// `name`'s pending upgrade; `None` if nothing is pending or `name` isn't registered.
    #[must_use]
    pub fn scheduled(&self, name: &SystemProgramName) -> Option<ScheduledUpgrade> {
        self.programs.get(name).copied().flatten()
    }
}

/// Where the [`Registry`] lives: a PDA of `system_upgrader`.
#[must_use]
pub fn registry_account_id() -> AccountId {
    AccountId::for_public_pda(&SYSTEM_UPGRADER_ACCOUNT_ID, &PdaSeed::new(REGISTRY_SEED))
}

/// The actor every `system_upgrader` message is sent to: the registry, under `system_upgrader`.
#[must_use]
pub fn registry_actor() -> Actor {
    Actor::new(registry_account_id(), SYSTEM_UPGRADER_ACCOUNT_ID)
}

/// The system program `name`'s header, under the program loader.
#[must_use]
pub fn loader_actor(name: &SystemProgramName) -> Actor {
    Actor::new(
        AccountId::from_system_program_name(name),
        PROGRAM_LOADER_ACCOUNT_ID,
    )
}

/// `system_upgrader`'s fixed address.
#[must_use]
pub const fn system_upgrader_account_id() -> AccountId {
    SYSTEM_UPGRADER_ACCOUNT_ID
}
