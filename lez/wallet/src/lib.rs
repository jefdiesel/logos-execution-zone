#![expect(
    clippy::print_stdout,
    reason = "This is a CLI application, printing to stdout and stderr is expected and convenient"
)]
#![expect(
    clippy::shadow_unrelated,
    reason = "Most of the shadows come from args parsing which is ok"
)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
};

pub use account_manager::{
    AccountIdentity, AccountMention, CIPHERTEXT_PAD_SIZE, SelectedActorState,
};
use anyhow::{Context as _, Result, ensure};
use bip39::Mnemonic;
use common::{HashType, PageLimit, block::Block, transaction::LeeTransaction};
use config::WalletConfig;
use futures::TryFutureExt as _;
use key_protocol::key_management::key_tree::chain_index::ChainIndex;
use lee::{
    Account, AccountId, EncryptedNote, PredictedCrossMessages, PrivacyPreservingTransaction,
    ProgramId, ProvingInput, PublicExecutionContext, Recipient, RecipientEncryption, Simulation,
    privacy_preserving_transaction::{
        circuit::{Dependency, ProgramCatalog, ProgramKind},
        message::Message,
    },
};
use lee_core::{
    BlockId, Commitment, CommitmentPosition, CommitmentSetDigest, EphemeralSecretKey,
    MembershipProof, MessageWitness, Nullifier, NullifierSecretKey, RootCall, SharedSecretKey,
    account::{Actor, Nonce},
    execution_state::TransactionEntry,
    native_token::NATIVE_TOKEN_PROGRAM_ID,
    program::{MessageBody, MessageData, Publication},
};
use log::warn;
use sequencer_service_rpc::{RpcClient as _, SequencerClient, SequencerClientBuilder};
use storage::{Storage, spent_nullifiers::SpentNullifiers};
use tokio::io::AsyncWriteExt as _;
use url::Url;

use crate::{
    account::{AccountIdWithPrivacy, Label},
    config::WalletConfigOverrides,
    poller::TxPoller,
    program_facades::program_loader::ProgramLoader,
    storage::key_chain::{SharedAccountDerivation, SharedAccountEntry, UserKeyChain},
};

pub mod account;
mod account_manager;
pub mod cli;
pub mod config;
pub mod helperfunctions;
pub mod poller;
pub mod program_facades;
pub mod storage;

pub const SUPPRESS_VERBOSE_PRINTS: &str = "SUPPRESS_VERBOSE_PRINTS";

pub const HOME_DIR_ENV_VAR: &str = "LEE_WALLET_HOME_DIR";

/// Default execution gas limit for wallet-built public transactions: roughly
/// three times the widest measured program call, one fifth of the per-block cap.
pub const DEFAULT_GAS_LIMIT: u64 = 2_000_000;

/// Base fee the default `max_fee` is sized against: 8x the genesis minimum,
/// so defaults survive early congestion without re-signing.
const ASSUMED_BASE_FEE: u128 = 64;

/// Serialized-size allowance the default `max_fee` is sized against.
const ASSUMED_DATA_BYTES: u128 = 100_000;

/// Default cap on the fee reservation for wallet-built public transactions.
pub const DEFAULT_MAX_FEE: u128 = max_fee_for(DEFAULT_GAS_LIMIT);

/// Info returned when creating a shared account.
pub struct SharedAccountInfo {
    pub account_id: AccountId,
    pub npk: lee_core::NullifierPublicKey,
    pub vpk: lee_core::encryption::ViewingPublicKey,
}

#[derive(Default)]
pub struct AsyncDelivery {
    pub recoveries: Vec<RecipientEncryption>,
    pub seals: Vec<Recipient>,
    pub promotions: Promotions,
}

// The async messages to execute at once: the listed candidates and, unless `listed_only`, each
// async message to an account this transaction witnesses.
#[derive(Default)]
pub struct Promotions {
    pub public: BTreeSet<u64>,
    pub private: BTreeSet<u64>,
    pub listed_only: bool,
}

// A pending message's body as this wallet reads it, and the recipient its recovery note or seal
// names.
pub struct PendingMessage {
    pub position: CommitmentPosition,
    pub publication: Publication,
    pub body: MessageBody,
    pub recipient: Recipient,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionFailureKind {
    #[error("Failed to get data from sequencer")]
    SequencerError(#[source] anyhow::Error),
    #[error("Inputs amounts does not match outputs")]
    AmountMismatchError,
    #[error("Accounts key not found")]
    KeyNotFoundError,
    #[error("Sequencer client error: {0}")]
    SequencerClientError(#[from] sequencer_service_rpc::ClientError),
    #[error("Can not pay for operation")]
    InsufficientFundsError,
    #[error("Account {0} data is invalid")]
    AccountDataError(AccountId),
    #[error("Account {0} is mentioned with conflicting identities")]
    ConflictingAccountIdentity(AccountId),
    #[error("Account {0} holds state but has no membership proof to update it")]
    MissingMembershipProof(AccountId),
    #[error("Account {0} is a foreign private account, which this wallet cannot witness")]
    ForeignPrivateAccount(AccountId),
    #[error("Program bytecode splits into {expected} segment(s) but {actual} were supplied")]
    SegmentCountMismatch { expected: usize, actual: usize },
    #[error("Program bytecode is not a valid RISC0 program binary")]
    InvalidProgramBinary(#[source] anyhow::Error),
    #[error(
        "Program uses a non-default kernel ELF; only programs built with the protocol's \
         default kernel can be deployed"
    )]
    UnsupportedKernelElf,
    #[error("Failed to build transaction: {0}")]
    TransactionBuildError(#[from] lee::error::LeeError),
    #[error("Failed to sign transaction: {0}")]
    SignError(anyhow::Error),
    #[error("Sending transaction failed for each client")]
    MultiSequencerTransactionSendError,
    #[error("Failed to store wallet data")]
    StorageError(#[source] anyhow::Error),
    #[error("Failed to join a task: {0}")]
    JoinError(#[from] tokio::task::JoinError),
}

/// A received message a transaction starts from. Its membership path comes with the proofs of the
/// transaction's accounts, so every action spends under one root.
struct Receipt {
    body: MessageBody,
    position: CommitmentPosition,
    rho: Option<[u8; 32]>,
    commitment: Commitment,
}

pub struct WalletCore {
    config_path: PathBuf,
    config_overrides: Option<WalletConfigOverrides>,
    config: WalletConfig,

    storage: Storage,
    storage_path: PathBuf,
    spent_nullifiers: SpentNullifiers,

    sequencer_client: SequencerClient,
}

impl WalletCore {
    /// Construct wallet using [`HOME_DIR_ENV_VAR`] env var for paths or user home dir if not set.
    pub fn from_env() -> Result<Self> {
        let config_path = helperfunctions::fetch_config_path()?;
        let storage_path = helperfunctions::fetch_persistent_storage_path()?;

        Self::new_update_chain(config_path, storage_path, None)
    }

    pub fn new_update_chain(
        config_path: PathBuf,
        storage_path: PathBuf,
        config_overrides: Option<WalletConfigOverrides>,
    ) -> Result<Self> {
        let storage = Storage::from_path(&storage_path)
            .with_context(|| format!("Failed to load storage from {}", storage_path.display()))?;

        Self::new(config_path, storage_path, config_overrides, storage)
    }

    pub fn new_init_storage(
        config_path: PathBuf,
        storage_path: PathBuf,
        config_overrides: Option<WalletConfigOverrides>,
        password: &str,
    ) -> Result<(Self, Mnemonic)> {
        let (storage, mnemonic) = Storage::new(password).context("Failed to create storage")?;
        let wallet = Self::new(config_path, storage_path, config_overrides, storage)?;

        Ok((wallet, mnemonic))
    }

    fn new(
        config_path: PathBuf,
        storage_path: PathBuf,
        config_overrides: Option<WalletConfigOverrides>,
        storage: Storage,
    ) -> Result<Self> {
        let mut config =
            WalletConfig::from_path_or_initialize_default(&config_path).with_context(|| {
                format!(
                    "Failed to deserialize wallet config at {}",
                    config_path.display()
                )
            })?;
        if let Some(config_overrides) = config_overrides.clone() {
            config.apply_overrides(config_overrides);
        }

        let spent_nullifiers = SpentNullifiers::open(storage_path.with_extension("nullifiers"))?;

        let mut builder = SequencerClientBuilder::default();
        if let Some(basic_auth) = &config.sequencer.basic_auth {
            builder = builder.set_headers(
                std::iter::once((
                    "Authorization".parse().expect("Header name is valid"),
                    format!("Basic {basic_auth}")
                        .parse()
                        .context("Invalid basic auth format")?,
                ))
                .collect(),
            );
        }
        let sequencer_client = builder
            .build(&config.sequencer.sequencer_addr)
            .context("Failed to create sequencer client")?;

        Ok(Self {
            config_path,
            config_overrides,
            config,
            storage,
            storage_path,
            spent_nullifiers,
            sequencer_client,
        })
    }

    /// Get configuration with applied overrides.
    #[must_use]
    pub const fn config(&self) -> &WalletConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: WalletConfig) {
        self.config = config;
    }

    #[must_use]
    pub fn poller(&self) -> TxPoller {
        TxPoller::new(self.config(), self.client_owned())
    }

    #[must_use]
    pub const fn client(&self) -> &SequencerClient {
        &self.sequencer_client
    }

    #[must_use]
    pub fn client_owned(&self) -> SequencerClient {
        self.sequencer_client.clone()
    }

    #[must_use]
    pub fn client_url(&self) -> Url {
        self.config.sequencer.sequencer_addr.clone()
    }

    /// Get storage.
    #[must_use]
    pub const fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Get mutable reference to storage.
    #[must_use]
    pub const fn storage_mut(&mut self) -> &mut Storage {
        &mut self.storage
    }

    /// Restore storage from an existing mnemonic phrase.
    pub fn restore_storage(&mut self, mnemonic: &Mnemonic, password: &str) -> Result<()> {
        self.storage.restore(mnemonic, password)
    }

    /// Store persistent data at home.
    pub fn store_persistent_data(&self) -> Result<()> {
        self.storage
            .save_to_path(&self.storage_path)
            .with_context(|| {
                format!(
                    "Failed to store persistent accounts at {}",
                    self.storage_path.display()
                )
            })?;

        println!(
            "Stored persistent accounts at {}",
            self.storage_path.display()
        );

        Ok(())
    }

    /// Store persistent data at home.
    pub async fn store_config_changes(&self) -> Result<()> {
        let config = serde_json::to_vec_pretty(&self.config)?;

        let mut config_file = tokio::fs::File::create(&self.config_path).await?;
        config_file.write_all(&config).await?;
        // Ensure data is flushed to disk before returning to prevent race conditions
        config_file.sync_all().await?;

        log::info!("Stored data at {}", self.config_path.display());

        Ok(())
    }

    pub fn create_new_account_public(
        &mut self,
        chain_index: Option<ChainIndex>,
    ) -> (AccountId, ChainIndex) {
        self.storage
            .key_chain_mut()
            .generate_new_public_transaction_private_key(chain_index)
    }

    pub fn create_private_accounts_key(&mut self, chain_index: Option<ChainIndex>) -> ChainIndex {
        self.storage
            .key_chain_mut()
            .create_private_accounts_key(chain_index)
    }

    pub fn create_new_account_private(
        &mut self,
        chain_index: Option<ChainIndex>,
    ) -> (AccountId, ChainIndex) {
        self.storage
            .key_chain_mut()
            .generate_new_privacy_preserving_transaction_key_chain(chain_index)
    }

    /// Insert a group key holder into storage.
    pub fn insert_group_key_holder(
        &mut self,
        name: Label,
        holder: key_protocol::key_management::group_key_holder::GroupKeyHolder,
    ) {
        self.storage
            .key_chain_mut()
            .insert_group_key_holder(name, holder);
    }

    /// Set the wallet's dedicated sealing secret key.
    pub const fn set_sealing_secret_key(
        &mut self,
        key: key_protocol::key_management::secret_holders::ViewingSecretKey,
    ) {
        self.storage.key_chain_mut().set_sealing_secret_key(key);
    }

    /// Resolve an `AccountId` to the appropriate `AccountIdentity` variant.
    /// Checks the key tree first, then shared private accounts.
    #[must_use]
    pub fn resolve_private_account(&self, account_id: lee::AccountId) -> Option<AccountIdentity> {
        // Check key tree first
        if self
            .storage
            .key_chain()
            .private_account(account_id)
            .is_some()
        {
            return Some(AccountIdentity::PrivateOwned(account_id));
        }

        // Check shared private accounts
        let entry = self
            .storage
            .key_chain()
            .shared_private_account(account_id)?;
        let keys = self.storage.key_chain().derive_shared_account_keys(entry)?;
        let vpk = keys.generate_viewing_public_key();

        Some(match entry.kind {
            lee_core::PrivateAccountKind::Pda {
                account_id: authority,
                seed,
            } => AccountIdentity::PrivatePdaShared {
                authority,
                seed,
                nsk: keys.nullifier_secret_key(),
                vpk,
            },
            lee_core::PrivateAccountKind::Regular => AccountIdentity::PrivateShared {
                ask: keys.authorization_secret_key,
                vpk,
            },
        })
    }

    /// Remove a group key holder from storage. Returns the removed holder if it existed.
    pub fn remove_group_key_holder(
        &mut self,
        name: &Label,
    ) -> Option<key_protocol::key_management::group_key_holder::GroupKeyHolder> {
        self.storage.key_chain_mut().remove_group_key_holder(name)
    }

    /// Records an account through `record`, at the state it has reached: the history from genesis
    /// to the chain's current tip, and at least to the last synced block, is scanned for its
    /// initialization's nullifier, and the nullifier pass decodes the init and every subsequent
    /// update. Nothing is recorded unless the scan covers every block and the record is saved.
    async fn record_caught_up(
        &mut self,
        record: impl FnOnce(&mut UserKeyChain) -> Result<()>,
        account_id: AccountId,
        nsk: &lee_core::NullifierSecretKey,
    ) -> Result<(), ExecutionFailureKind> {
        let tip = self
            .get_last_block_id()
            .await
            .map_err(ExecutionFailureKind::SequencerError)?
            .max(self.storage.last_synced_block());
        log::info!("Scanning account {account_id:#?} from genesis to block {tip}");
        let poller = self.poller();
        let unrecorded = self.storage.key_chain().clone();
        self.storage
            .key_chain_mut()
            .record_caught_up(
                record,
                account_id,
                nsk,
                tip,
                poller.poll_block_range(1..=tip),
            )
            .await
            .map_err(ExecutionFailureKind::SequencerError)?;
        self.store_persistent_data().map_err(|err| {
            *self.storage.key_chain_mut() = unrecorded;
            ExecutionFailureKind::StorageError(err)
        })
    }

    /// Create a shared PDA account from a group's GMS. Returns the `AccountId` and derived keys.
    pub async fn create_shared_pda_account(
        &mut self,
        group_name: Label,
        pda_seed: lee_core::program::PdaSeed,
        program_id: lee_core::program::ProgramId,
    ) -> Result<SharedAccountInfo> {
        let holder = self
            .storage
            .key_chain()
            .group_key_holder(&group_name)
            .context(format!("Group '{group_name}' not found"))?;

        let keys = holder.derive_keys_for_pda(&program_id, &pda_seed);
        let npk = keys.generate_nullifier_public_key();
        let vpk = keys.generate_viewing_public_key();
        let account_id = AccountId::for_private_pda(
            &AccountId::from_builtin_program(program_id),
            &pda_seed,
            &npk,
            &vpk,
        );

        let entry = SharedAccountEntry {
            group_label: group_name,
            derivation: SharedAccountDerivation::Pda {
                seed: pda_seed,
                program_id,
            },
            kind: lee_core::PrivateAccountKind::Pda {
                account_id: AccountId::from_builtin_program(program_id),
                seed: pda_seed,
            },
            account: Account::default(),
        };
        self.record_caught_up(
            |key_chain| {
                key_chain.insert_shared_private_account(account_id, entry);
                Ok(())
            },
            account_id,
            &keys.nullifier_secret_key(),
        )
        .await?;

        Ok(SharedAccountInfo {
            account_id,
            npk,
            vpk,
        })
    }

    /// Create a shared regular private account from a group's GMS under the given wallet-local
    /// `derivation_id`.
    pub async fn create_shared_regular_account(
        &mut self,
        group_name: Label,
        derivation_id: [u8; 32],
    ) -> Result<SharedAccountInfo> {
        let holder = self
            .storage
            .key_chain()
            .group_key_holder(&group_name)
            .context(format!("Group '{group_name}' not found"))?;

        let keys = holder.derive_regular_shared_account_keys(&derivation_id);
        let npk = keys.generate_nullifier_public_key();
        let vpk = keys.generate_viewing_public_key();
        let account_id = AccountId::from((&npk, &vpk));

        let entry = SharedAccountEntry {
            group_label: group_name,
            derivation: SharedAccountDerivation::Regular { derivation_id },
            kind: lee_core::PrivateAccountKind::Regular,
            account: Account::default(),
        };
        self.record_caught_up(
            |key_chain| {
                key_chain.insert_shared_private_account(account_id, entry);
                Ok(())
            },
            account_id,
            &keys.nullifier_secret_key(),
        )
        .await?;

        Ok(SharedAccountInfo {
            account_id,
            npk,
            vpk,
        })
    }

    /// Get account balance.
    pub async fn get_account_balance(&self, acc: AccountId) -> Result<u128> {
        Ok(self.sequencer_client.get_account_balance(acc).await?)
    }

    /// Get accounts nonces.
    pub async fn get_accounts_nonces(&self, accs: &[AccountId]) -> Result<Vec<Nonce>> {
        Ok(self
            .sequencer_client
            .get_accounts_nonces(accs.to_vec())
            .await?)
    }

    /// Returns the account's nonce and the selected actor state; its balance is the actor state at
    /// the native token program.
    pub async fn get_account_view(&self, actor_state_selector: Actor) -> Result<Option<Account>> {
        // RPC projections include empty actor states; the wallet omits them.
        Ok(self
            .sequencer_client
            .get_account_view(actor_state_selector)
            .await?
            .map(|mut account| {
                account
                    .data
                    .actor_states
                    .retain(|_, actor_state| !actor_state.is_empty());
                account
            }))
    }

    pub async fn get_publications(
        &self,
        from_position: CommitmentPosition,
        limit: PageLimit,
    ) -> Result<Vec<(CommitmentPosition, Publication)>> {
        Ok(self
            .sequencer_client
            .get_publications(from_position, limit)
            .await?)
    }

    // Messages that one of this wallet's key pairs opens: a private address's recovery note, or a
    // sealed message.
    pub async fn owned_pending_messages(&mut self) -> Result<Vec<PendingMessage>> {
        const PAGE: PageLimit = 256;
        self.sync_to_latest_block().await?;
        let mut records = Vec::new();
        let mut from_position = 0;
        loop {
            let page = self.get_publications(from_position, PAGE).await?;
            let Some((last, _)) = page.last() else {
                return Ok(records);
            };
            from_position = last.saturating_add(1);
            records.extend(
                self.unreceived(
                    page.into_iter()
                        .filter_map(|(position, publication)| self.read(position, publication)),
                ),
            );
        }
    }

    // The body, recipient, sealed record's commitment randomness and nullifier key of a
    // publication one of this wallet's key pairs opens.
    fn private_receipt(
        &self,
        publication: &Publication,
    ) -> Option<(MessageBody, Recipient, Option<[u8; 32]>, NullifierSecretKey)> {
        match publication {
            Publication::Clear { body, recovery } => {
                let (recipient, nsk) = self
                    .storage
                    .key_chain()
                    .recover(body.to.account_id, recovery)?;
                Some((body.clone(), recipient, None, nsk))
            }
            Publication::Sealed(sealed) => {
                let (body, recipient, rho, nsk) = self.storage.key_chain().open(sealed)?;
                Some((body, recipient, Some(rho), nsk))
            }
        }
    }

    fn read(
        &self,
        position: CommitmentPosition,
        publication: Publication,
    ) -> Option<(PendingMessage, Nullifier)> {
        let (body, recipient, _, nsk) = self.private_receipt(&publication)?;
        let receipt = Nullifier::for_message(&nsk, &publication.commitment(), position);
        Some((
            PendingMessage {
                position,
                publication,
                body,
                recipient,
            },
            receipt,
        ))
    }

    pub async fn find_pending_message(
        &mut self,
        position: CommitmentPosition,
    ) -> Result<Option<PendingMessage>> {
        self.sync_to_latest_block().await?;
        let found = self
            .get_publications(position, 1)
            .await?
            .into_iter()
            .filter(|(found, _)| *found == position)
            .filter_map(|(found, publication)| self.read(found, publication));
        Ok(self.unreceived(found).last())
    }

    // Drops the records whose receipt this wallet sees spent.
    fn unreceived(
        &self,
        records: impl IntoIterator<Item = (PendingMessage, Nullifier)>,
    ) -> impl Iterator<Item = PendingMessage> {
        records
            .into_iter()
            .filter(|(_, receipt)| !self.spent_nullifiers.contains(receipt))
            .map(|(pending, _)| pending)
    }

    pub async fn get_account(&self, account_id: AccountIdWithPrivacy) -> Result<Account> {
        match account_id {
            AccountIdWithPrivacy::Public(acc_id) => {
                Ok(self.get_account_public(acc_id).await?.unwrap_or_default())
            }
            AccountIdWithPrivacy::Private(acc_id) => {
                if let Some(account) = self.get_account_private(acc_id) {
                    Ok(account)
                } else {
                    anyhow::bail!("Private account with id {acc_id} not found in storage")
                }
            }
        }
    }

    pub async fn get_last_block_id(&self) -> Result<u64> {
        Ok(self.sequencer_client.get_last_block_id().await?)
    }

    pub async fn get_block(&self, block_id: u64) -> Result<Option<Block>> {
        Ok(self.sequencer_client.get_block(block_id).await?)
    }

    pub async fn get_transaction(
        &self,
        hash: HashType,
    ) -> Result<Option<(LeeTransaction, BlockId)>> {
        Ok(self.sequencer_client.get_transaction(hash).await?)
    }

    /// Get public account.
    pub async fn get_account_public(&self, account_id: AccountId) -> Result<Option<Account>> {
        Ok(self.sequencer_client.get_account(account_id).await?)
    }

    pub async fn get_recovery_binding(&self, address: AccountId) -> Result<Option<EncryptedNote>> {
        Ok(self.sequencer_client.get_recovery_binding(address).await?)
    }

    #[must_use]
    pub fn get_account_public_signing_key(
        &self,
        account_id: AccountId,
    ) -> Option<&lee::PrivateKey> {
        self.storage.key_chain().pub_account_signing_key(account_id)
    }

    #[must_use]
    pub fn get_account_private(&self, account_id: AccountId) -> Option<Account> {
        self.private_account_state(account_id).cloned()
    }

    #[must_use]
    pub fn private_account_state(&self, account_id: AccountId) -> Option<&Account> {
        self.storage
            .key_chain()
            .private_account(account_id)
            .map(|acc| acc.account)
            .or_else(|| {
                self.storage
                    .key_chain()
                    .shared_private_account(account_id)
                    .map(|entry| &entry.account)
            })
    }

    #[must_use]
    pub fn get_private_account_commitment(&self, account_id: AccountId) -> Option<Commitment> {
        self.private_account_state(account_id)
            .map(|account| Commitment::new(&account_id, account))
    }

    pub async fn get_program_ids(&self) -> Result<BTreeMap<String, ProgramId>> {
        Ok(self.sequencer_client.get_program_ids().await?)
    }

    /// Poll transactions.
    pub async fn poll_transaction(&self, tx_hash: HashType) -> Result<(LeeTransaction, BlockId)> {
        self.poller().poll_tx(tx_hash).await
    }

    pub async fn get_proofs_and_root(
        &self,
        commitments: &[Commitment],
        message_position: Option<CommitmentPosition>,
    ) -> Result<(
        Vec<Option<MembershipProof>>,
        Option<MembershipProof>,
        CommitmentSetDigest,
    )> {
        Ok(self
            .sequencer_client
            .get_proofs_and_root(commitments.to_vec(), message_position)
            .await?)
    }

    pub(crate) async fn await_inclusion(
        &self,
        tx_hash: HashType,
    ) -> Result<(LeeTransaction, BlockId)> {
        println!("Transaction hash is {tx_hash}");
        let (tx, block_id) = self.poll_transaction(tx_hash).await?;
        println!("Transaction is included in block {block_id}");
        if std::env::var_os(SUPPRESS_VERBOSE_PRINTS).is_none() {
            println!("Transaction data is {tx:?}");
        }
        Ok((tx, block_id))
    }

    // The included transaction updates the recorded accounts whose current nullifier it spends;
    // the sync cursor stays where it was.
    pub(crate) async fn finish_transaction(
        &mut self,
        tx_hash: HashType,
    ) -> Result<cli::SubcommandReturnValue> {
        let (tx, block_id) = self.await_inclusion(tx_hash).await?;
        if let LeeTransaction::PrivacyPreserving(tx) = tx {
            let key_chain = self.storage.key_chain_mut();
            let mut index = key_chain.build_latest_nullifier_index();
            key_chain.sync_updates_via_nullifiers(&tx.message, &mut index);
        }
        self.store_persistent_data().with_context(|| {
            format!("Transaction {tx_hash} is included in block {block_id}, but storing the wallet failed")
        })?;
        Ok(cli::SubcommandReturnValue::TransactionExecuted { tx_hash })
    }

    pub async fn send_privacy_preserving_tx(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        programs: &ProgramCatalog,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        self.send_privacy_preserving_tx_with_pre_check(
            accounts,
            root,
            message,
            programs,
            |_| Ok(()),
        )
        .await
    }

    pub async fn send_tx(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        programs: &ProgramCatalog,
        cross_messages: Option<PredictedCrossMessages>,
        async_delivery: AsyncDelivery,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        self.send_tx_with_pre_check(
            accounts,
            root,
            message,
            programs,
            cross_messages,
            async_delivery,
            |_| Ok(()),
        )
        .await
    }

    // Public when every account is, no recovery binding is new and no candidate is selected,
    // privacy-preserving otherwise. A proof derives its boundary from current public state unless
    // `cross_messages` gives a conditional promise that settlement checks against live execution.
    #[expect(
        clippy::too_many_arguments,
        reason = "send_tx's arguments and the check of the accounts it reads"
    )]
    pub async fn send_tx_with_pre_check(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        programs: &ProgramCatalog,
        cross_messages: Option<PredictedCrossMessages>,
        async_delivery: AsyncDelivery,
        tx_pre_check: impl FnOnce(&[SelectedActorState]) -> Result<(), ExecutionFailureKind>,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        let to = root_actor(&accounts, root)?;
        let async_delivery = AsyncDelivery {
            recoveries: self.unbound(async_delivery.recoveries).await?,
            ..async_delivery
        };
        if async_delivery.recoveries.is_empty()
            && async_delivery.seals.is_empty()
            && async_delivery.promotions.public.is_empty()
            && async_delivery.promotions.private.is_empty()
            && !accounts.iter().any(|mention| mention.identity.is_private())
        {
            self.send_public(accounts, to, message, None, tx_pre_check)
                .await
                .map(|tx_hash| (tx_hash, Vec::new()))
        } else {
            self.send_proven(
                accounts,
                TransactionEntry::Sync(RootCall { to, message }),
                async_delivery,
                programs,
                cross_messages,
                tx_pre_check,
            )
            .await
        }
    }

    // A binding already on chain is reused: settlement attaches it to every publication.
    async fn unbound(
        &self,
        recoveries: Vec<RecipientEncryption>,
    ) -> Result<Vec<RecipientEncryption>, ExecutionFailureKind> {
        let mut unbound: Vec<RecipientEncryption> = Vec::new();
        for recovery in recoveries {
            let address = recovery.recipient.address();
            let bound = self
                .get_recovery_binding(address)
                .await
                .map_err(ExecutionFailureKind::SequencerError)?;
            if bound.is_none()
                && !unbound
                    .iter()
                    .any(|kept| kept.recipient.address() == address)
            {
                unbound.push(recovery);
            }
        }
        Ok(unbound)
    }

    // What settlement needs for an async message to `destination`: a public account to execute it,
    // declared, or a private account's recovery binding to publish it.
    pub fn async_destination(
        &self,
        destination: AccountMention,
    ) -> Result<(Option<AccountMention>, AsyncDelivery), ExecutionFailureKind> {
        if destination.identity.is_public() {
            return Ok((
                Some(AccountMention {
                    identity: destination.identity.without_signing(),
                    ..destination
                }),
                AsyncDelivery::default(),
            ));
        }
        Ok((
            None,
            AsyncDelivery {
                recoveries: self
                    .recipients(&destination)?
                    .into_iter()
                    .map(|recipient| RecipientEncryption {
                        recipient,
                        esk: EphemeralSecretKey(account_manager::random_bytes()),
                    })
                    .collect(),
                ..AsyncDelivery::default()
            },
        ))
    }

    // An async message that private execution sends to `destination`: a private destination's
    // recipient material seals it, and no recovery binding publishes the destination.
    pub fn seal_destination(
        &self,
        destination: AccountMention,
    ) -> Result<(Option<AccountMention>, AsyncDelivery), ExecutionFailureKind> {
        if destination.identity.public_account_id().is_some() {
            return self.async_destination(destination);
        }
        Ok((
            None,
            AsyncDelivery {
                seals: self.recipients(&destination)?,
                ..AsyncDelivery::default()
            },
        ))
    }

    // A private destination's recipient material at each blinded address it is mentioned at, or at
    // its canonical address.
    fn recipients(
        &self,
        destination: &AccountMention,
    ) -> Result<Vec<Recipient>, ExecutionFailureKind> {
        let recipient = account_manager::recipient(self, &destination.identity)
            .ok_or(ExecutionFailureKind::KeyNotFoundError)?;
        if destination.address_openings.is_empty() {
            return Ok(vec![recipient]);
        }
        Ok(destination
            .address_openings
            .iter()
            .map(|opening| Recipient {
                opening: Some(*opening),
                ..recipient.clone()
            })
            .collect())
    }

    // `async_destination` for each of `async_destinations` and `seal_destination` for each of
    // `seal_destinations`, joined.
    pub fn async_destinations(
        &self,
        async_destinations: Vec<AccountMention>,
        seal_destinations: Vec<AccountMention>,
    ) -> Result<(Vec<AccountMention>, AsyncDelivery), ExecutionFailureKind> {
        let mut declared = Vec::new();
        let mut joined = AsyncDelivery::default();
        let destinations = async_destinations
            .into_iter()
            .map(|destination| self.async_destination(destination))
            .chain(
                seal_destinations
                    .into_iter()
                    .map(|destination| self.seal_destination(destination)),
            );
        for destination in destinations {
            let (
                mention,
                AsyncDelivery {
                    recoveries, seals, ..
                },
            ) = destination?;
            declared.extend(mention);
            joined.recoveries.extend(recoveries);
            joined.seals.extend(seals);
        }
        Ok((declared, joined))
    }

    pub async fn send_privacy_preserving_tx_with_pre_check(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        programs: &ProgramCatalog,
        tx_pre_check: impl FnOnce(&[SelectedActorState]) -> Result<(), ExecutionFailureKind>,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        let root = TransactionEntry::Sync(RootCall {
            to: root_actor(&accounts, root)?,
            message,
        });
        self.send_proven(
            accounts,
            root,
            AsyncDelivery::default(),
            programs,
            None,
            tx_pre_check,
        )
        .await
    }

    async fn send_proven(
        &self,
        accounts: Vec<AccountMention>,
        root: TransactionEntry<Receipt>,
        async_delivery: AsyncDelivery,
        programs: &ProgramCatalog,
        cross_messages: Option<PredictedCrossMessages>,
        tx_pre_check: impl FnOnce(&[SelectedActorState]) -> Result<(), ExecutionFailureKind>,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        let AsyncDelivery {
            recoveries,
            seals,
            promotions:
                Promotions {
                    public,
                    private,
                    listed_only,
                },
        } = async_delivery;
        let message = match &root {
            TransactionEntry::Async(receipt) => Some((receipt.position, receipt.commitment)),
            TransactionEntry::Sync(_) => None,
        };
        let acc_manager = account_manager::AccountManager::new(self, accounts, message).await?;

        tx_pre_check(&acc_manager.selected_actor_states())?;

        for account_id in acc_manager.accounts_outgrowing_pad() {
            warn!(
                "Account {account_id} exceeds the {CIPHERTEXT_PAD_SIZE}-byte note pad; its note is \
                 identifiable by length in this transaction"
            );
        }

        let shared_secrets = acc_manager.shared_secrets();
        let input = ProvingInput {
            root: root.map(|Receipt { body, rho, .. }| MessageWitness {
                body,
                rho,
                membership_proof: acc_manager
                    .message_path()
                    .expect("a received message's path is fetched with its accounts")
                    .clone(),
                filler: account_manager::dummy_output(),
            }),
            public_context: PublicExecutionContext {
                promotions: public,
                ..PublicExecutionContext::new(acc_manager.public_actors(), acc_manager.signers())
            },
            private_witnesses: acc_manager.private_witnesses()?,
            dummy_inputs: acc_manager.dummy_inputs_default(),
            ciphertext_padding: Some(CIPHERTEXT_PAD_SIZE),
            recoveries,
            private_promotions: private,
        };
        let witnessed: HashSet<AccountId> = input
            .private_witnesses
            .iter()
            .flat_map(|witness| {
                let account_id = witness.account_id();
                std::iter::once(account_id).chain(
                    witness
                        .address_openings
                        .iter()
                        .map(move |opening| account_id.blinded(opening)),
                )
            })
            .collect();
        let select =
            move |_, body: &MessageBody| !listed_only && witnessed.contains(&body.to.account_id);
        let choose_seal = move |body: &MessageBody| {
            let to = body.to.account_id;
            seals
                .iter()
                .find(|recipient| recipient.address() == to)
                .map(|recipient| RecipientEncryption {
                    recipient: recipient.clone(),
                    esk: EphemeralSecretKey(account_manager::random_bytes()),
                })
                .ok_or_else(|| {
                    lee::error::LeeError::InvalidInput(format!(
                        "No recipient material seals an async message to {to}"
                    ))
                })
        };

        let programs = programs.clone();
        let present = acc_manager.presenter();
        let (output, proof) = match cross_messages {
            None => {
                let simulation = Simulation {
                    public_actor_states: acc_manager.public_actor_states(),
                    admitted_accounts: Some(acc_manager.admitted_accounts()),
                };
                tokio::task::spawn_blocking(move || {
                    lee::execute_and_prove(
                        input,
                        &simulation,
                        &programs,
                        present,
                        select,
                        choose_seal,
                    )
                })
            }
            Some(cross_messages) => tokio::task::spawn_blocking(move || {
                lee::execute_and_prove_with_cross_messages(
                    input,
                    cross_messages,
                    &programs,
                    present,
                    select,
                    choose_seal,
                )
            }),
        }
        .await??;

        let message = Message {
            admission_evidence: acc_manager.admission_evidence(),
            ..Message::from_circuit_output(acc_manager.public_account_nonces(), output)
        };

        let message_hash = message.hash();
        let signatures_public_keys = acc_manager
            .sign_message(message_hash)
            .map_err(ExecutionFailureKind::SignError)?;

        let witness_set =
            lee::privacy_preserving_transaction::witness_set::WitnessSet::from_raw_parts(
                signatures_public_keys,
                proof,
            );

        let tx = PrivacyPreservingTransaction::new(message, witness_set);

        let call_res = self
            .sequencer_client
            .send_transaction(LeeTransaction::PrivacyPreserving(tx))
            .await;

        Ok((call_res?, shared_secrets))
    }

    pub async fn send_pub_tx(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
    ) -> Result<HashType, ExecutionFailureKind> {
        self.send_pub_tx_paid_by(accounts, root, message, None)
            .await
    }

    /// Like [`Self::send_pub_tx`], but `payer` (if given) covers the fee instead of the wallet's
    /// self-pay selection. See [`Self::send_pub_tx_with_pre_check`].
    pub async fn send_pub_tx_paid_by(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        payer: Option<AccountId>,
    ) -> Result<HashType, ExecutionFailureKind> {
        self.send_pub_tx_with_pre_check(accounts, root, message, payer, |_| Ok(()))
            .await
    }

    /// Sends a public transaction over `accounts`, paid by `payer` if given.
    ///
    /// `payer: None` picks the first funded signing account in `accounts`, or the first signing
    /// account if none is funded (see [`AccountManager::fee_payer_account_id`]).
    ///
    /// An explicit payer may be one of `accounts`' signing entries, or any other public account
    /// whose signing key the wallet holds: the latter co-signs (nonce and signature appended
    /// after `accounts`' own) without joining the message's `account_ids`, so programs with a
    /// fixed account shape (like the `program_loader`, whose accounts are all freshly claimed
    /// and unfunded) can still be paid for.
    pub async fn send_pub_tx_with_pre_check(
        &self,
        accounts: Vec<AccountMention>,
        root: usize,
        message: MessageData,
        payer: Option<AccountId>,
        tx_pre_check: impl FnOnce(&[SelectedActorState]) -> Result<(), ExecutionFailureKind>,
    ) -> Result<HashType, ExecutionFailureKind> {
        let to = root_actor(&accounts, root)?;
        self.send_public(accounts, to, message, payer, tx_pre_check)
            .await
    }

    async fn send_public(
        &self,
        accounts: Vec<AccountMention>,
        to: Actor,
        message: MessageData,
        payer: Option<AccountId>,
        tx_pre_check: impl FnOnce(&[SelectedActorState]) -> Result<(), ExecutionFailureKind>,
    ) -> Result<HashType, ExecutionFailureKind> {
        // Public transaction, all accounts must be public
        if accounts.iter().any(|mention| mention.identity.is_private()) {
            return Err(ExecutionFailureKind::TransactionBuildError(
                lee::error::LeeError::InvalidInput(
                    "Private accounts are not allowed in public transactions".to_owned(),
                ),
            ));
        }

        let mut acc_manager = account_manager::AccountManager::new(self, accounts, None).await?;

        tx_pre_check(&acc_manager.selected_actor_states())?;

        let public_actors = acc_manager.public_actors();
        let account_ids = acc_manager.public_account_ids();
        let mut nonces = acc_manager.public_account_nonces();

        let invalid_input = |msg: &str| {
            ExecutionFailureKind::TransactionBuildError(lee::error::LeeError::InvalidInput(
                msg.to_owned(),
            ))
        };
        let (payer, co_signer) = match payer {
            None => (
                acc_manager
                    .fee_payer_account_id(self)
                    .await?
                    .ok_or_else(|| {
                        invalid_input("Public transaction has no signing account to pay its fees")
                    })?,
                None,
            ),
            Some(payer) if acc_manager.signs_for(payer) => (payer, None),
            Some(payer) if account_ids.contains(&payer) => {
                return Err(invalid_input(
                    "Fee payer is a non-signing account of this transaction",
                ));
            }
            Some(payer) => {
                let key = self.get_account_public_signing_key(payer).ok_or_else(|| {
                    invalid_input("Fee payer's signing key is not held by this wallet")
                })?;
                let account = self
                    .get_account_view(Actor::native_balance(payer))
                    .await
                    .map_err(ExecutionFailureKind::SequencerError)?
                    .unwrap_or_default();
                nonces.insert(payer, account.nonce);
                (payer, Some(key))
            }
        };

        let message = lee::public_transaction::Message::new(
            to,
            message,
            public_actors,
            nonces,
            Some(lee::FeeDeclaration::new(
                payer,
                self.config.gas_limit,
                0,
                max_fee_for(self.config.gas_limit),
            )),
            acc_manager.admission_evidence(),
        );

        let message_hash = message.hash();
        let mut signatures_public_keys = acc_manager
            .sign_message(message_hash)
            .map_err(ExecutionFailureKind::SignError)?;
        if let Some(key) = co_signer {
            signatures_public_keys.push((
                lee::Signature::new(key, &message_hash),
                lee::PublicKey::new_from_private_key(key),
            ));
        }

        let witness_set =
            lee::public_transaction::WitnessSet::from_raw_parts(signatures_public_keys);

        let tx = lee::public_transaction::PublicTransaction::new(message, witness_set);

        Ok(self
            .sequencer_client
            .send_transaction(LeeTransaction::Public(tx))
            .await?)
    }

    pub async fn receive_pending_message(
        &mut self,
        pending: PendingMessage,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        self.receive_pending_message_with(
            pending,
            Vec::new(),
            &ProgramCatalog::default(),
            None,
            AsyncDelivery::default(),
        )
        .await
    }

    // Executes the message as published. Its recipient takes part as `accounts` declare its
    // account, unauthorized otherwise, and each declared program missing from `programs` runs
    // from its deployment. Authorization is per account: authorizing the recipient's account
    // authorizes the published message too.
    pub async fn receive_pending_message_with(
        &mut self,
        PendingMessage {
            position,
            publication,
            ..
        }: PendingMessage,
        mut accounts: Vec<AccountMention>,
        programs: &ProgramCatalog,
        cross_messages: Option<PredictedCrossMessages>,
        async_delivery: AsyncDelivery,
    ) -> Result<(HashType, Vec<SharedSecretKey>), ExecutionFailureKind> {
        let Some((body, recipient, rho, nsk)) = self.private_receipt(&publication) else {
            return Err(ExecutionFailureKind::TransactionBuildError(
                lee::error::LeeError::InvalidInput(format!(
                    "No key pair of this wallet opens the message published at {position}"
                )),
            ));
        };
        let account_id = recipient.account_id();
        if self.resolve_private_account(account_id).is_none() {
            self.record_caught_up(
                |key_chain| key_chain.record_received(&recipient),
                account_id,
                &nsk,
            )
            .await?;
        }
        let identity = self
            .resolve_private_account(account_id)
            .ok_or(ExecutionFailureKind::KeyNotFoundError)?;
        let receiver = Actor::new(account_id, body.to.program_account_id);
        let index = accounts
            .iter()
            .position(|mention| mention.actor() == receiver)
            .unwrap_or_else(|| {
                let mention = accounts
                    .iter()
                    .find(|mention| mention.identity.account_id() == account_id)
                    .map_or_else(
                        || {
                            identity
                                .select_program_actor_state(receiver.program_account_id)
                                .without_authorization()
                        },
                        |declared| AccountMention {
                            program_account_id: receiver.program_account_id,
                            ..declared.clone()
                        },
                    );
                accounts.insert(0, mention);
                0
            });
        accounts[index].address_openings.extend(recipient.opening);

        let mut programs = programs.clone();
        let declared: BTreeSet<AccountId> = accounts
            .iter()
            .map(|mention| mention.program_account_id)
            .collect();
        // The native program runs as protocol code.
        for program in declared {
            if program != NATIVE_TOKEN_PROGRAM_ID
                && !programs.programs.contains_key(&program)
                && let Some(deployed) = ProgramLoader(self)
                    .program(program)
                    .await
                    .map_err(ExecutionFailureKind::SequencerError)?
            {
                programs.programs.insert(
                    program,
                    Dependency {
                        program: deployed,
                        kind: ProgramKind::Disclosed,
                    },
                );
            }
        }

        let async_delivery = AsyncDelivery {
            recoveries: self.unbound(async_delivery.recoveries).await?,
            ..async_delivery
        };
        let receipt = Receipt {
            body,
            position,
            rho,
            commitment: publication.commitment(),
        };
        self.send_proven(
            accounts,
            TransactionEntry::Async(receipt),
            async_delivery,
            &programs,
            cross_messages,
            |_| Ok(()),
        )
        .await
    }

    pub async fn sync_to_latest_block(&mut self) -> Result<u64> {
        let latest_block_id = self.get_last_block_id().await?;
        println!("Latest block is {latest_block_id}");
        self.sync_to_block(latest_block_id).await?;
        Ok(latest_block_id)
    }

    pub async fn sync_to_block(&mut self, block_id: u64) -> Result<()> {
        self.sync(block_id, false).await
    }

    /// Syncs to the latest block like [`Self::sync_to_latest_block`], also trial-decrypting with
    /// every owned key pair each note no followed nullifier claims. Restoration needs this for the
    /// private PDAs under its keys, which no record names.
    pub async fn restore_to_latest_block(&mut self) -> Result<()> {
        let latest_block_id = self.get_last_block_id().await?;
        self.sync(latest_block_id, true).await
    }

    /// Rediscovers the private accounts this wallet's keys hold from genesis through the current
    /// tip, keeping every other record, the sync cursor and the spent-nullifier cache. A failed
    /// scan changes nothing.
    pub async fn rescan_private_accounts(&mut self) -> Result<()> {
        let tip = self
            .get_last_block_id()
            .await?
            .max(self.storage.last_synced_block());
        log::info!("Rescanning private accounts from genesis to block {tip}");
        let poller = self.poller();
        let rescanned = self
            .storage
            .key_chain()
            .clone()
            .rescan(tip, poller.poll_block_range(1..=tip))
            .await?;
        let unscanned = std::mem::replace(self.storage.key_chain_mut(), rescanned);
        self.store_persistent_data().inspect_err(|_| {
            *self.storage.key_chain_mut() = unscanned;
        })
    }

    pub async fn restore_keys(&mut self, depth: u32) -> Result<()> {
        self.storage.key_chain_mut().generate_trees_for_depth(depth);

        println!(
            "Public tree generated\n\
             Private tree generated"
        );

        self.restore_to_latest_block().await?;

        // A key a pending message names stays, though the account it credits is not initialized
        // yet.
        let awaited = self
            .owned_pending_messages()
            .await?
            .into_iter()
            .map(|pending| {
                lee::AccountId::for_regular_private_account(
                    &pending.recipient.npk,
                    &pending.recipient.vpk,
                )
            })
            .collect();
        let leader_client = self.client_owned();

        self.storage
            .key_chain_mut()
            .cleanup_trees_remove_uninit_layered(
                depth,
                |account_id| {
                    leader_client
                        .get_account(account_id)
                        .map_ok(Option::unwrap_or_default)
                        .map_err(Into::into)
                },
                &awaited,
            )
            .await?;

        println!(
            "Public tree cleaned up\n\
             Private tree cleaned up"
        );

        self.store_persistent_data()?;

        Ok(())
    }

    async fn sync(&mut self, block_id: u64, discover: bool) -> Result<()> {
        use futures::TryStreamExt as _;

        // The cursor and the spent-nullifier cache claim the same blocks.
        let cursor = self.storage.last_synced_block();
        self.spent_nullifiers.keep(cursor)?;
        self.storage
            .set_last_synced_block(cursor.min(self.spent_nullifiers.covered()));
        let last_synced_block = self.storage.last_synced_block();
        if last_synced_block >= block_id {
            return Ok(());
        }

        let before_polling = std::time::Instant::now();
        let num_of_blocks = block_id.saturating_sub(last_synced_block);
        if num_of_blocks == 0 {
            return Ok(());
        }

        println!("Syncing to block {block_id}. Blocks to sync: {num_of_blocks}");

        let poller = self.poller();
        let mut blocks =
            std::pin::pin!(poller.poll_block_range(last_synced_block.saturating_add(1)..=block_id));

        // Watch every recorded account, initialized or not, for its next transition's nullifier.
        let mut index = self.storage.key_chain().build_latest_nullifier_index();
        let bar = indicatif::ProgressBar::new(num_of_blocks);
        let mut expected = last_synced_block.saturating_add(1)..=block_id;
        while let Some(block) = blocks.try_next().await? {
            ensure!(
                expected.next() == Some(block.header.block_id),
                "History reaches block {} out of sequence",
                block.header.block_id
            );
            let mut spent = Vec::new();
            for tx in block.body.transactions {
                let LeeTransaction::PrivacyPreserving(pp_tx) = &tx else {
                    continue;
                };
                spent.extend(
                    pp_tx
                        .message
                        .execution
                        .private_actions
                        .iter()
                        .map(|action| action.nullifier),
                );
                let key_chain = self.storage.key_chain_mut();
                let followed = key_chain.sync_updates_via_nullifiers(&pp_tx.message, &mut index);
                if discover {
                    key_chain.discover_unfollowed(&pp_tx.message, &followed, &mut index);
                }
            }

            self.spent_nullifiers.append(spent)?;
            self.storage.set_last_synced_block(block.header.block_id);
            self.store_persistent_data()?;
            bar.inc(1);
        }
        bar.finish();

        println!(
            "Synced to block {block_id} in {:?}",
            before_polling.elapsed()
        );

        Ok(())
    }

    #[must_use]
    pub const fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    #[must_use]
    pub const fn storage_path(&self) -> &PathBuf {
        &self.storage_path
    }

    #[must_use]
    pub const fn config_overrides(&self) -> &Option<WalletConfigOverrides> {
        &self.config_overrides
    }
}

/// Sizes a fee cap for a given gas limit: a wallet that raises its gas limit
/// must raise its fee cap in step, or the reservation cannot cover the gas.
#[must_use]
#[expect(
    clippy::as_conversions,
    reason = "u128::from is not const; the widening is lossless"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "gas_limit and ASSUMED_DATA_BYTES both fit well within u128::MAX, so the widened \
              sum and product cannot overflow"
)]
pub const fn max_fee_for(gas_limit: u64) -> u128 {
    (gas_limit as u128 + ASSUMED_DATA_BYTES) * ASSUMED_BASE_FEE
}

fn decrypt_note_at(
    message: &Message,
    i: usize,
    secret: &SharedSecretKey,
) -> Option<(lee_core::PrivateAccountKind, Account)> {
    lee_core::EncryptionScheme::decrypt(
        &message.execution.private_actions[i]
            .encrypted_post_state
            .ciphertext,
        secret,
        &message.execution.private_actions[i].nullifier,
    )
}

// The root is one of the transaction's accounts, so it runs with the placement declared for it.
fn root_actor(accounts: &[AccountMention], root: usize) -> Result<Actor, ExecutionFailureKind> {
    accounts
        .get(root)
        .map(AccountMention::actor)
        .ok_or_else(|| {
            ExecutionFailureKind::TransactionBuildError(lee::error::LeeError::InvalidInput(
                format!("Root index {root} is not one of the transaction's accounts"),
            ))
        })
}

#[cfg(test)]
mod tests {
    use std::{ffi::CString, str::FromStr as _};

    use bip39::Mnemonic;

    #[test]
    fn mnemonic_roundtrip() {
        let mnemonic =
            Mnemonic::from_entropy(&[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]).unwrap();

        let c_mnemonic_string = CString::new(mnemonic.to_string()).unwrap();
        let c_mnemonic_string_raw = c_mnemonic_string.into_raw();
        // Safety: Will be safe, pointer is created from CString
        let c_str = unsafe { CString::from_raw(c_mnemonic_string_raw) };
        let mn_string = c_str.to_str().unwrap();

        let mn_ret = Mnemonic::from_str(mn_string).unwrap();

        assert_eq!(mnemonic, mn_ret);
    }
}
