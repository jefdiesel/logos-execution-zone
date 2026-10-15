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
    /// `66nUvzJtTxA729aUpTcAWDrb7cV9Heb2bDzceHQhLYad`, fetched 2026-10-15.
    const JEF_RECORD_HEX: &str = concat!(
        "03f14ac788881df29017740ab2d0b382811fc305efaffda30d97f5cfd04c8d128976da6a97a80100001c0500",
        "005de3b987a8ff5bd756993dbda51ff5cf5ea32ea68225889a2d17b82c8fae1afc6a6ff15f045be6d8a92230",
        "3504a74246ab1cb7f30b7376c63bf431eb174f6cfd1237e2831397bd85d8fcfadc05da1213e9f23073eba96a",
        "d9cff637dbce0a479a30adfc6cf107ba51925907a81385347051e9c48e77eabff534df42bc1251ae4aebfb42",
        "c7fd1f25b7e3d75d35e3893b8d365740cfc3d39b3ab0f80d77672e8fcb50bc7d2d77797d90548424a1e8caf2",
        "2f748c075f883200db54fe4606bbfda4c107413d5ded6ec95faf1c1afd4c68d2963d1d152db31b6c5fd7e2e4",
        "10872b1e116ad97090cc7517ca9252f2134360a0292de87bde9679c2c7469259a628d279839d90709aa62f48",
        "ebfb622458b83638345c7f8f28696d0ce8c63975b1ca1eb6721bb8140b64852eccf88e7a1bb9411157038319",
        "55036315076d51d2d844d22e68c55bc417e3ec5177a7e4d81f8a9a9bd3a804a640a6c16aa259ccb022bba713",
        "58ef49f00fee5f755789a92bdf4f9a0cec272d4e5b81018813017a2f545881d76bc587279e54d5f3ef3906b4",
        "5bdebe47b502f56795e616e366d55f2e8ff0b98448b87116a30205adce61de5a55845c0376963c2989208a09",
        "fca8ca886a51bb6e4bf5a77a3a735762c9b2826a760efc21d1db9bf7ba103895ebe700d95405d398e7d34c84",
        "de002e1e15eb0dd567fe554aae1fc8fe9f2e65f12265ca5ce85e6a8063ae8841022887161006d04986f63864",
        "76fcd6f073e26ee38547b91ad5caa38f55e357bc1c55ee2dca5ad23754957052adb405e01425cd87d6b95b79",
        "dc451910b1aa802acd0ac27cbcf23e9bad2b986c94035d9998f7b7b3caadcfa318abbb093c0b645f83a964ff",
        "8e8c9e2522289f1bdb7db736aea957cb4cdd8687d7b6e3168b7c539713a54342a848ce03adcc98bf85c84507",
        "0ddb7ce8070d974bfefafb1d3dec32f29408a85a4bddcb727f6ce28c88b25b1cab3eba6c6b1a1c81db308863",
        "b4fe89ae21a8d0bfc64644a35da4e0a8064cd5935e6cfada35af52b9509b02ceb81eaba4b21aa7838b246252",
        "a7e1d22d17f76177ecb10c6bb7b80999aabf81dda90e98dcb24ed2eeba58e7ebd74275fe6f17f843344cd91b",
        "bdd33f303071b2ecf2a5ce706f03126bc68b417e515400e6812ace51bb58d8f637fafc90e272c1e13afe06b4",
        "d8c6362ca6479b7b41d116556cf693e9d6f15542fa76e25fb6a6a587402a19b49c6c99b6f904b475c0735f81",
        "17760e78a62d896f892893c25d44ee79d723cf926365e1a999bf994474e7f65b510f2369e3b858a3c8ac6c44",
        "4c919187434ef288d4c20029ceef7b09bbe3065aa3ec01c05a58538f94985263b93827e9a0d63393fadb0f9a",
        "ff399a3f6f1bc10ed4fde693ebe0dbb25236cd0742245d8493e60e8c1cbe227ff5b154901dfefa9bb187ca60",
        "a9e279b51ee2c74cd9fdb70519c62482d0b3bc9c6abfeeb0a3725beed30f2d3df99ec9a22c1887f3436d3cba",
        "a6e7fcbc0c96c18d295ef676231a763ec22e165af12dc71c68f9a478a7386acd65a1ceec55a2e44eeecde958",
        "14c304ef9038b7f4e29213b5d6e2c2bc9acac12cfd31f3abb0bf55b37de6f7b226e1fee6d9e7b78c1806fbd4",
        "8ddfac8b8b90af3da0254edcd676b62f404bf05d79cc3b6d208fd1115b2f263e2fe60dae9776e0d7e86afb2c",
        "26c719bf6e014310c84b96cf1aee38e7e85fe52834c13e2eae1ceb63ff70989632b52de3ddbeb2ddbd47c76e",
        "d440430f79c44e96bb741f85dd89cae2f3826e4e73dbaec2c87c82555c7890a2918f3932310d496c6915bb9f",
        "2ef7f639ffd0add168016ac8fc049cb15f56cd739fb603f517274637d982d38450",
    );
    const NOW: u64 = 1_792_100_000_000; // 2026-10-15

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
        let registry: AccountId = "66nUvzJtTxA729aUpTcAWDrb7cV9Heb2bDzceHQhLYad"
            .parse()
            .unwrap();
        let (id, _) = derive("jef");
        assert_eq!(
            record_address(&registry, &id).to_string(),
            "88ex9Tiz63e7isrm3TFrTVR6FERT8jH8B1NTkQ3WuUgA"
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
