use std::{collections::BTreeMap, sync::Arc};

use anyhow::Result;
use sequencer_executor_actor::protocol::{
    GetAccount, GetAccountNonces, GetBlock, GetBlockRange, GetLastBlockId, GetProofsAndRoot,
    GetTransaction, Transaction, TransactionOrigin,
};
use wallet::WalletSequencerClient;

use crate::sequencer::SequencerServiceFFI;

#[derive(Clone)]
pub struct SequencerServiceFFIWrapper(pub Arc<&'static SequencerServiceFFI>);

impl SequencerServiceFFIWrapper {
    pub fn wrap(sequencer: &'static SequencerServiceFFI) -> Self {
        Self(Arc::new(sequencer))
    }
}

impl WalletSequencerClient for SequencerServiceFFIWrapper {
    fn from_config(_config: &wallet::config::WalletConfig) -> Result<Self> {
        anyhow::bail!("Not intended way");
    }

    async fn get_account(&self, account_id: lee::AccountId) -> Result<lee::Account> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetAccount { account_id })
            .send()
            .await?
            .account)
    }

    async fn get_account_balance(&self, account_id: lee::AccountId) -> Result<u128> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetAccount { account_id })
            .send()
            .await?
            .account
            .data
            .native_balance()?)
    }

    async fn get_accounts_nonces(
        &self,
        account_ids: Vec<lee::AccountId>,
    ) -> Result<Vec<lee_core::account::Nonce>> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetAccountNonces { account_ids })
            .send()
            .await?)
    }

    async fn get_block(&self, block_id: lee_core::BlockId) -> Result<Option<common::block::Block>> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetBlock { block_id })
            .send()
            .await?)
    }

    async fn get_block_range(
        &self,
        start_block_id: lee_core::BlockId,
        end_block_id: lee_core::BlockId,
    ) -> Result<Vec<common::block::Block>> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetBlockRange {
                range: (start_block_id..=end_block_id).try_into()?,
            })
            .send()
            .await?)
    }

    async fn get_last_block_id(&self) -> Result<lee_core::BlockId> {
        Ok(self.0.executor_ref().ask(GetLastBlockId {}).send().await?)
    }

    async fn get_transaction(
        &self,
        tx_hash: common::HashType,
    ) -> Result<Option<(common::transaction::LeeTransaction, lee_core::BlockId)>> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetTransaction { tx_hash })
            .send()
            .await?)
    }

    async fn send_transaction(
        &self,
        tx: common::transaction::LeeTransaction,
    ) -> Result<common::HashType> {
        let tx_hash = tx.hash();

        self.0
            .executor_ref()
            .ask(Transaction {
                transaction: tx,
                origin: TransactionOrigin::User,
            })
            .send()
            .await?;

        Ok(tx_hash)
    }

    async fn get_account_view(
        &self,
        shard_selector: lee::ProgramShardSelector,
    ) -> Result<lee::Account> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetAccount {
                account_id: shard_selector.account_id,
            })
            .send()
            .await?
            .account
            .project([shard_selector.program_account_id]))
    }

    async fn get_program_ids(&self) -> Result<std::collections::BTreeMap<String, lee::ProgramId>> {
        // TODO: Sequencer RPC don't read programs from state. Need to update both.
        let mut program_ids = BTreeMap::new();
        program_ids.insert(
            "privacy_preserving_circuit".to_owned(),
            lee::PRIVACY_PRESERVING_CIRCUIT_ID,
        );
        Ok(program_ids)
    }

    async fn get_proofs_and_root(
        &self,
        commitments: Vec<lee_core::Commitment>,
    ) -> Result<(
        Vec<Option<lee_core::MembershipProof>>,
        lee_core::CommitmentSetDigest,
    )> {
        Ok(self
            .0
            .executor_ref()
            .ask(GetProofsAndRoot { commitments })
            .send()
            .await?)
    }
}
