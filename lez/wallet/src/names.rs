//! Paying a `.lez` name: resolving it through an LNS (Logos Names) registry to the
//! private receiving keys, or the public account, its holder published.
//!
//! The chain never sees a name. A name is turned into an identifier and a key on
//! this side; the registry keeps, at an address derived from the identifier, a
//! record whose profile is sealed under the key. Specification:
//! <https://github.com/jefdiesel/logos-names/blob/main/SPEC.md>.

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use anyhow::{Context as _, Result, anyhow, bail};
use borsh::BorshDeserialize;
use lee_core::{
    NullifierPublicKey, PrivateAccountKind,
    account::{AccountId, Actor},
    encryption::ViewingPublicKey,
    program::PdaSeed,
};
use sha2::{Digest as _, Sha256};

use crate::{AccountIdentity, WalletCore};

const MASTER_SALT: &[u8] = b"logos-names/v2";
const MASTER_ITERATIONS: u32 = 100_000;
const ID_DOMAIN: &[u8] = b"logos-names/v2/id";
const KEY_DOMAIN: &[u8] = b"logos-names/v2/key";
const RECORD_SEED_DOMAIN: &[u8] = b"logos-names/v2/name/";
const RECORD_ASSOCIATED_DATA: &[u8] = b"logos-names/v2/record";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// A lapsed name still resolves for this long, then anyone may register it.
const GRACE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const SUFFIX: &str = ".lez";

/// The on-chain record of a name.
#[derive(Debug, BorshDeserialize)]
struct NameRecord {
    _version: u8,
    _owner_tag: [u8; 32],
    expires: u64,
    sealed: Vec<u8>,
}

/// What a name resolves to, sealed inside its record.
#[derive(Debug, BorshDeserialize)]
struct Profile {
    name: String,
    account: Option<[u8; 32]>,
    npk: Option<[u8; 32]>,
    vpk: Vec<u8>,
    _texts: Vec<(String, String)>,
}

/// Where a payment to a name goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameDestination {
    /// The name's private receiving keys: the chain sees neither recipient nor amount.
    Private { npk: [u8; 32], vpk: Vec<u8> },
    /// A public account the holder chose to publish.
    Public(AccountId),
}

impl NameDestination {
    pub fn into_identity(self) -> Result<AccountIdentity> {
        Ok(match self {
            Self::Private { npk, vpk } => AccountIdentity::PrivateForeign {
                npk: NullifierPublicKey(npk),
                vpk: ViewingPublicKey::from_bytes(vpk)
                    .map_err(|e| anyhow!("the name publishes an invalid viewing key: {e}"))?,
                kind: PrivateAccountKind::Regular,
            },
            Self::Public(account) => AccountIdentity::PublicNoSign(account),
        })
    }
}

/// `jef.lez` and `JEF` both mean the name `jef`.
#[must_use]
pub fn normalize(name: &str) -> String {
    let name = name.trim().to_lowercase();
    name.strip_suffix(SUFFIX).unwrap_or(&name).to_owned()
}

/// The name's public identifier and its secret key.
#[must_use]
pub fn derive(name: &str) -> ([u8; 32], [u8; 32]) {
    let master =
        pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(name.as_bytes(), MASTER_SALT, MASTER_ITERATIONS);
    (
        domain_hash(ID_DOMAIN, &master),
        domain_hash(KEY_DOMAIN, &master),
    )
}

/// The account holding the record of the name with identifier `id` in `registry`.
#[must_use]
pub fn record_address(registry: &AccountId, id: &[u8; 32]) -> AccountId {
    AccountId::for_public_pda(registry, &PdaSeed::new(domain_hash(RECORD_SEED_DOMAIN, id)))
}

/// Resolves `name` through `registry` to where a payment should go: the private keys
/// when the name has them, else its public account.
pub async fn resolve(
    wallet: &WalletCore,
    registry: AccountId,
    name: &str,
    now_ms: u64,
) -> Result<NameDestination> {
    let name = normalize(name);
    let (id, key) = derive(&name);
    let address = record_address(&registry, &id);
    let account = wallet
        .get_account_view(Actor::new(address, registry))
        .await
        .context("failed to read the name's record")?;
    let bytes = account
        .map(|account| account.data.actor_state(registry).to_vec())
        .unwrap_or_default();
    destination_from_record(&name, &key, &bytes, now_ms)
}

/// Decodes a record's bytes into where a payment to `name` goes.
pub fn destination_from_record(
    name: &str,
    key: &[u8; 32],
    record: &[u8],
    now_ms: u64,
) -> Result<NameDestination> {
    if record.is_empty() {
        bail!("{name}{SUFFIX} is not registered");
    }
    let record =
        NameRecord::try_from_slice(record).context("the registry holds a malformed record")?;
    if now_ms >= record.expires.saturating_add(GRACE_MS) {
        bail!("{name}{SUFFIX} has expired");
    }
    let profile = open_profile(key, &record.sealed)
        .filter(|profile| profile.name == name)
        .ok_or_else(|| anyhow!("{name}{SUFFIX} has no readable record"))?;
    match (profile.npk, profile.vpk.is_empty(), profile.account) {
        (Some(npk), false, _) => Ok(NameDestination::Private {
            npk,
            vpk: profile.vpk,
        }),
        (_, _, Some(account)) => Ok(NameDestination::Public(AccountId::new(account))),
        _ => bail!("{name}{SUFFIX} has no payment destination"),
    }
}

fn open_profile(key: &[u8; 32], sealed: &[u8]) -> Option<Profile> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
    let plaintext = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: RECORD_ASSOCIATED_DATA,
            },
        )
        .ok()?;
    // The profile is zero-padded; decode from the start and ignore the padding.
    Profile::deserialize(&mut plaintext.as_slice()).ok()
}

fn domain_hash(domain: &[u8], data: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(domain)
        .chain_update(data)
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `jef`'s record on the LEZ v0.3.1 testnet registry
    /// `2PXg3SSmh5hvECTz4xcfV2d5YvxwyF6yVhpnjTokzCkX`, fetched 2026-10-16.
    const JEF_RECORD_HEX: &str = concat!(
        "04f14ac788881df29017740ab2d0b382811fc305efaffda30d97f5cfd04c8d12892fd05b9ca80100001c0500",
        "005639544cfd5db0b24de6742fb54530bf7df45b6d445e5f22a93cc132f275a618017967d2bca1ae1a2efd1f",
        "518802f95c831a3990083e047be664d072bc66b149c133ee3347ad8b6e775a1d883a3c7a0a6bdcaf9eef2238",
        "e19fc73235d05e1cd2873fadfc9bc092265efefdca85a06989346c61df7631776d2693082c7c8e088ae62b05",
        "cfdf784a2d1a9ae95aca9f3d4b5b79789147a3d5675b232b6ee96fa49ef3bec0fca14d3330301f3529b9cf90",
        "18c13e2bedd0d0c4727938443468d1f162a28d32a031da9078f3b118f7744d7133862704f5c29896b78a4194",
        "972699fd3816e18e820287b3b7232040d12c7dc294a220acb5a17ab9b0bccec8ed21a47b2eeb34cf709d13c1",
        "166bcd14f034623af10dca2e33a751bb54d1628f6fba4537fcf61f2b306fc35538aae381381af6ea825cc308",
        "be79c4439ec3a3aa105eb300271f8be145bb904aa831333236e0252510871706a4e642bf2cabe733e6c95795",
        "3960bcb0c3a2358bc0ca44b0ce6d8b295bb3ba9e1c235f6db985ae8d9108dd611c708bafc0b0c41accea4423",
        "fab2c832018963112a947ac195fdf2c4a6313362d259c819d28999ed08e07c2b4fbaa5be0f5b26499ec41c64",
        "d2b96646639ff0ee6bbe1dec9e84b841f6c44f6be55d48f2ff0e77f85ad02c12cbcb2c7e78afaaf64070754c",
        "5e749874922d2b3e36ae173a710d387bcb1d0c42324960ef2abd716f4b6f963c48f236946fb386248586cb15",
        "da8c986698e0fec3dee4d435eb79bd5ec5d755ce513fb1ed20ab2eadc87ad498fab2ab45f818e4a4d4ed5abf",
        "098ca128d468b117baa7fa88e65a242e1e0b586e8d83357eed12443b296e1e7f89a076ad5f8acd84951d9d3c",
        "0f7b8c057e65845a9e24c7a07d3f3017524d6d08ec84139bea7ef22fa286965b5e9c6ee9b01eef84569bed37",
        "f2fc26c40533ab0cc366c93916748a2925fc25e981d9d6a6c9e1f0131bcb820ea22eb6a0b1bdf9ec4be29b3e",
        "88b8508ffbd77a135ad0cecbbffedd78d07f4d715cdeb73c97f8a92d0b2b9542b4b7c69e57409b429700ca82",
        "460b7b122d8653ae183cfa93b66f8966d6e7ced0ae43cecd01801202a39e9c57c2059124de408107d9e6e529",
        "55329340f0d2c6130757aaa67f206c39cf39239a836c9c8abbfe3d6ed58cef44cff7e59662a8bffbacb0ec09",
        "6790862ede6e9e392a68f0b3b3154f27eb3592ddc004d1bf50d189afa3213445cce3739feb0eff14fd095b29",
        "b60949afa2d526295727418edc63fddc5a98f5f70c07ccb41765848c69f4a70081afa85a63b9b1af2b56ac19",
        "b36923c774867a3254319ed3b520fd9ac72113601fcae5d25cf8902388edc4921835a05637133f2cd894ce14",
        "7426c7aa8d30e88c93c998408cf5cd996df6dfce18442a2cd1e4c8ffc1916c8bea00828dd73e395a023e9d92",
        "6a4445514a00802d33abd2b9d1011d35e908d873799c3f74b36ae5272b73fdace91714089b3af15b6b8a0a3e",
        "b34b55e8b697a27623c1809f7388a06b48c65720014528927d1dd6b0421a2add2f52b59a4f39ec54b85fc32a",
        "9096f218ea7effc1e6979f591890890ef16e63d192c86388d44a4ffca062963de595182dff46f62d6477427e",
        "976b9de0a3bea0aa923ab83e3ad430f0aaef7fcef1c2769848b690f9b8440ffb47875568d712281bdba713eb",
        "14fdd2bf7dee2ce8e367220a89dc3efe7eb3fe945b824e051d56764511e51637f6fa6ac82203618f1ccae961",
        "0fb84589b70b0f59e5e8611abef082914a4f9c84007cd790dad4255cc70425f1643e8008c608e86e7c7097ec",
        "98929326f99704e0cdb41719f9191781ff5248ca213c28d9fa33bdd7e1c5240b4c",
    );
    const NOW: u64 = 1_792_160_000_000; // 2026-10-16

    fn jef_record() -> Vec<u8> {
        hex::decode(JEF_RECORD_HEX).unwrap()
    }

    #[test]
    fn derives_the_specification_vectors() {
        let (id, key) = derive("jef");
        assert_eq!(
            hex::encode(id),
            "86d7e719e7acabe344ef8a69782b72ed0c21087e73e400e7056ab0daf7e6c618"
        );
        assert_eq!(
            hex::encode(key),
            "d19aa10c57273a3ff4f569cc3a9c35819b2a9bcac7ba8a48deb131269cb6e2e0"
        );
    }

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize(" JEF.lez "), "jef");
        assert_eq!(normalize("jef"), "jef");
    }

    #[test]
    fn finds_the_testnet_record_address() {
        let registry: AccountId = "2PXg3SSmh5hvECTz4xcfV2d5YvxwyF6yVhpnjTokzCkX"
            .parse()
            .unwrap();
        let (id, _) = derive("jef");
        assert_eq!(
            record_address(&registry, &id).to_string(),
            "3pNoRoWnhMacEP2LvfjHYkvDso8URHsioZ8hwEN73D6E"
        );
    }

    #[test]
    fn resolves_a_real_record_to_private_keys() {
        let (_, key) = derive("jef");
        let NameDestination::Private { vpk, .. } =
            destination_from_record("jef", &key, &jef_record(), NOW).unwrap()
        else {
            panic!("jef publishes private receiving keys");
        };
        assert_eq!(vpk.len(), 1184);
    }

    #[test]
    fn refuses_the_wrong_name_an_empty_record_and_a_lapsed_name() {
        let (_, other_key) = derive("logos");
        assert!(destination_from_record("jef", &other_key, &jef_record(), NOW).is_err());
        let (_, key) = derive("jef");
        assert!(destination_from_record("jef", &key, &[], NOW).is_err());
        let far_future = 1_900_000_000_000;
        assert!(destination_from_record("jef", &key, &jef_record(), far_future).is_err());
    }
}
