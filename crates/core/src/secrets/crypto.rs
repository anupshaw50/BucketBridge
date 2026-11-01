//! Key derivation and the AES-256-GCM envelope format used by the vault.
//!
//! # Key derivation
//!
//! The 256-bit master key is `Argon2id(password, salt)` (Argon2 version 0x13)
//! with the costs stored in [`KdfParams`]. New passwords use
//! [`KdfCost::RECOMMENDED`]; stored parameters are always honoured, so the
//! recommendation can be raised later without breaking existing data.
//!
//! # Envelope
//!
//! An envelope is standard base64 (with padding) of
//!
//! ```text
//! magic 0xBB | version 0x01 | nonce (12 bytes) | ciphertext | GCM tag (16 bytes)
//! ```
//!
//! The nonce is fresh random for every encryption. The associated data binds
//! the header and the purpose of the envelope:
//!
//! ```text
//! "BucketBridge vault" 0x00 | magic | version | purpose | 0x00 | context
//! ```
//!
//! where purpose `secret` has the secret reference as context, so a ciphertext
//! moved to another reference fails to open, and purpose `verifier` has the
//! canonical KDF parameters as context.

use std::fmt;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use zeroize::{Zeroize, Zeroizing};

use super::SecretString;
use crate::config::{KdfParams, SecurityConfig};
use crate::error::{Error, Result};

/// The only supported KDF.
pub(super) const KDF_ALGORITHM: &str = "argon2id";

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const MIN_SALT_LEN: usize = 16;
const MAX_SALT_LEN: usize = 64;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const MAGIC: u8 = 0xBB;
const VERSION: u8 = 0x01;
const HEADER_LEN: usize = 2;
const AAD_DOMAIN: &[u8] = b"BucketBridge vault";
const VERIFIER_PLAINTEXT: &[u8] = b"BucketBridge master password verifier";

/// Longest accepted master password, in bytes.
pub(super) const MAX_PASSWORD_LEN: usize = 1024;

// Bounds for stored parameters: a damaged or hostile config.toml must not be
// able to make unlocking allocate unbounded memory or spin for hours.
const MAX_M_COST_KIB: u32 = 2 * 1024 * 1024;
const MAX_T_COST: u32 = 64;
const MAX_P_COST: u32 = 64;

/// Argon2id cost used when a master password is set or changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfCost {
    /// Memory in KiB; at least `8 * p_cost`, at most 2 GiB.
    pub m_cost_kib: u32,
    /// Passes over the memory; 1 to 64.
    pub t_cost: u32,
    /// Lanes; 1 to 64.
    pub p_cost: u32,
}

impl KdfCost {
    /// 256 MiB of memory, 3 passes, 4 lanes.
    ///
    /// Chosen with the procedure of RFC 9106, section 4: as much memory and
    /// time as one call can afford. Unlocking happens once per session, so
    /// about a third of a second is acceptable: about 0.36 s on one core of a
    /// 2020 ARM64 laptop in a release build (the lanes are computed
    /// sequentially). 256 MiB is a transient allocation that 4 GB machines can
    /// afford. It is four times the memory of the RFC's option for
    /// memory-constrained environments (64 MiB, 3 passes, 4 lanes), which
    /// takes only about 85 ms on the same core.
    pub const RECOMMENDED: Self = Self {
        m_cost_kib: 256 * 1024,
        t_cost: 3,
        p_cost: 4,
    };
}

impl Default for KdfCost {
    fn default() -> Self {
        Self::RECOMMENDED
    }
}

/// A derived master key; zeroized on drop.
pub(super) struct MasterKey(Zeroizing<[u8; KEY_LEN]>);

impl MasterKey {
    fn cipher(&self) -> Result<Aes256Gcm> {
        Aes256Gcm::new_from_slice(self.0.as_slice()).map_err(|_| internal("invalid key length"))
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

/// Why an envelope could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnvelopeError {
    /// The text is not standard base64.
    NotBase64,
    /// Too short to hold a header, a nonce and a tag.
    Truncated,
    /// The magic byte is missing: not produced by the vault.
    NotAnEnvelope,
    /// Produced by a format version this build cannot read.
    UnsupportedVersion(u8),
    /// Wrong key, wrong reference or modified data.
    IntegrityCheckFailed,
    /// The decrypted bytes are not UTF-8.
    NotUtf8,
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBase64 => f.write_str("the stored value is not base64"),
            Self::Truncated => f.write_str("the stored value is truncated"),
            Self::NotAnEnvelope => f.write_str("the stored value is not an encrypted envelope"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported envelope version {version}")
            }
            Self::IntegrityCheckFailed => f.write_str(
                "integrity check failed (the data was modified, moved from another \
                 reference, or encrypted under a different master password)",
            ),
            Self::NotUtf8 => f.write_str("the decrypted value is not UTF-8"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

#[derive(Clone, Copy)]
enum Purpose {
    Secret,
    Verifier,
}

impl Purpose {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Secret => b"secret",
            Self::Verifier => b"verifier",
        }
    }
}

/// Checks a master password supplied by the user.
pub(super) fn check_password(password: &str) -> Result<()> {
    if password.is_empty() {
        return Err(usage("the master password must not be empty"));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(usage(&format!(
            "the master password must not be longer than {MAX_PASSWORD_LEN} bytes"
        )));
    }
    Ok(())
}

/// Checks a cost for new passwords.
pub(super) fn check_cost(cost: KdfCost) -> Result<()> {
    argon2_params(cost.m_cost_kib, cost.t_cost, cost.p_cost).map(|_| ())
}

/// Fresh parameters with a random salt for a new master password.
pub(super) fn new_kdf_params(cost: KdfCost) -> Result<KdfParams> {
    check_cost(cost)?;
    let mut salt = [0u8; SALT_LEN];
    fill_random(&mut salt)?;
    Ok(KdfParams {
        algorithm: KDF_ALGORITHM.to_owned(),
        m_cost_kib: cost.m_cost_kib,
        t_cost: cost.t_cost,
        p_cost: cost.p_cost,
        salt_b64: STANDARD.encode(salt),
    })
}

/// Structural validation of a persisted [`SecurityConfig`].
pub(super) fn check_security_config(config: &SecurityConfig) -> Result<()> {
    check_kdf_params(&config.kdf)?;
    let blob = STANDARD
        .decode(&config.verifier_b64)
        .map_err(|_| invalid_config("the verifier is not base64"))?;
    match blob.as_slice() {
        [MAGIC, VERSION, rest @ ..]
            if rest.len() == NONCE_LEN + VERIFIER_PLAINTEXT.len() + TAG_LEN =>
        {
            Ok(())
        }
        _ => Err(invalid_config("the verifier is malformed")),
    }
}

/// Derives the master key. Blocks for the configured cost.
pub(super) fn derive_key(password: &str, kdf: &KdfParams) -> Result<MasterKey> {
    check_password(password)?;
    let (params, salt) = check_kdf_params(kdf)?;
    let block_count = params.block_count();
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut blocks: Vec<Block> = Vec::new();
    blocks
        .try_reserve_exact(block_count)
        .map_err(|_| out_of_memory(kdf.m_cost_kib))?;
    blocks.resize(block_count, Block::new());
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let result = argon2.hash_password_into_with_memory(
        password.as_bytes(),
        &salt,
        key.as_mut_slice(),
        &mut blocks,
    );
    // The working memory holds password-derived state: wipe it before freeing.
    blocks.zeroize();
    result.map_err(|error| internal(&format!("key derivation failed: {error}")))?;
    Ok(MasterKey(key))
}

/// The verifier stored next to the KDF parameters.
pub(super) fn make_verifier(key: &MasterKey, kdf: &KdfParams) -> Result<String> {
    seal(
        key,
        Purpose::Verifier,
        &kdf_context(kdf),
        VERIFIER_PLAINTEXT,
    )
}

/// Whether `key` opens the verifier of `config`. `false` means the password
/// is wrong, or the configuration was modified.
pub(super) fn verify(key: &MasterKey, config: &SecurityConfig) -> Result<bool> {
    match open(
        key,
        Purpose::Verifier,
        &kdf_context(&config.kdf),
        &config.verifier_b64,
    ) {
        Ok(plaintext) => Ok(plaintext.as_slice() == VERIFIER_PLAINTEXT),
        Err(EnvelopeError::IntegrityCheckFailed) => Ok(false),
        Err(error) => Err(invalid_config(&format!(
            "the verifier cannot be read: {error}"
        ))),
    }
}

/// Encrypts the secret stored under `reference`.
pub(super) fn seal_secret(key: &MasterKey, reference: &str, value: &str) -> Result<String> {
    seal(key, Purpose::Secret, reference, value.as_bytes())
}

/// Decrypts the envelope stored under `reference`.
pub(super) fn open_secret(
    key: &MasterKey,
    reference: &str,
    envelope: &str,
) -> std::result::Result<SecretString, EnvelopeError> {
    let mut plaintext = open(key, Purpose::Secret, reference, envelope)?;
    // Move the buffer out without copying; the emptied wrapper wipes nothing.
    let bytes = std::mem::take(&mut *plaintext);
    match String::from_utf8(bytes) {
        Ok(text) => Ok(SecretString::new(text)),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            Err(EnvelopeError::NotUtf8)
        }
    }
}

fn seal(key: &MasterKey, purpose: Purpose, context: &str, plaintext: &[u8]) -> Result<String> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let nonce = Nonce::<Aes256Gcm>::from(nonce);
    let aad = associated_data(purpose, context);
    let ciphertext = key
        .cipher()?
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| internal("encryption failed"))?;
    let mut blob = Vec::with_capacity(HEADER_LEN + NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&[MAGIC, VERSION]);
    blob.extend_from_slice(nonce.as_slice());
    blob.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(blob))
}

fn open(
    key: &MasterKey,
    purpose: Purpose,
    context: &str,
    envelope: &str,
) -> std::result::Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let blob = STANDARD
        .decode(envelope)
        .map_err(|_| EnvelopeError::NotBase64)?;
    let (header, rest) = blob
        .split_at_checked(HEADER_LEN)
        .ok_or(EnvelopeError::Truncated)?;
    if header[0] != MAGIC {
        return Err(EnvelopeError::NotAnEnvelope);
    }
    if header[1] != VERSION {
        return Err(EnvelopeError::UnsupportedVersion(header[1]));
    }
    if rest.len() < NONCE_LEN + TAG_LEN {
        return Err(EnvelopeError::Truncated);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce = <&Nonce<Aes256Gcm>>::try_from(nonce).map_err(|_| EnvelopeError::Truncated)?;
    let aad = associated_data(purpose, context);
    let cipher = key
        .cipher()
        .map_err(|_| EnvelopeError::IntegrityCheckFailed)?;
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| EnvelopeError::IntegrityCheckFailed)
}

fn associated_data(purpose: Purpose, context: &str) -> Vec<u8> {
    let label = purpose.label();
    let mut aad =
        Vec::with_capacity(AAD_DOMAIN.len() + HEADER_LEN + label.len() + context.len() + 2);
    aad.extend_from_slice(AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(&[MAGIC, VERSION]);
    aad.extend_from_slice(label);
    aad.push(0);
    aad.extend_from_slice(context.as_bytes());
    aad
}

/// Canonical text of the KDF parameters, authenticated by the verifier.
fn kdf_context(kdf: &KdfParams) -> String {
    format!(
        "{};m={};t={};p={};salt={}",
        kdf.algorithm, kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, kdf.salt_b64
    )
}

fn check_kdf_params(kdf: &KdfParams) -> Result<(Params, Vec<u8>)> {
    if kdf.algorithm != KDF_ALGORITHM {
        return Err(invalid_config(&format!(
            "unsupported key derivation algorithm {:?}",
            kdf.algorithm
        )));
    }
    let params = argon2_params(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost)
        .map_err(|error| invalid_config(&error.to_string()))?;
    let salt = STANDARD
        .decode(&kdf.salt_b64)
        .map_err(|_| invalid_config("the salt is not base64"))?;
    if !(MIN_SALT_LEN..=MAX_SALT_LEN).contains(&salt.len()) {
        return Err(invalid_config(&format!(
            "the salt must be {MIN_SALT_LEN} to {MAX_SALT_LEN} bytes"
        )));
    }
    Ok((params, salt))
}

fn argon2_params(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Result<Params> {
    if !(1..=MAX_P_COST).contains(&p_cost) {
        return Err(usage(&format!("Argon2 lanes must be 1 to {MAX_P_COST}")));
    }
    if !(1..=MAX_T_COST).contains(&t_cost) {
        return Err(usage(&format!("Argon2 passes must be 1 to {MAX_T_COST}")));
    }
    if m_cost_kib < 8 * p_cost || m_cost_kib > MAX_M_COST_KIB {
        return Err(usage(&format!(
            "Argon2 memory must be 8 KiB per lane up to {MAX_M_COST_KIB} KiB"
        )));
    }
    Params::new(m_cost_kib, t_cost, p_cost, Some(KEY_LEN))
        .map_err(|error| usage(&format!("invalid Argon2 parameters: {error}")))
}

fn fill_random(buffer: &mut [u8]) -> Result<()> {
    getrandom::fill(buffer)
        .map_err(|error| internal(&format!("the OS random number generator failed: {error}")))
}

// Error constructors of this module.

fn usage(message: &str) -> Error {
    Error::Usage(message.to_owned())
}

fn invalid_config(reason: &str) -> Error {
    Error::Usage(format!(
        "the master password configuration is invalid: {reason}"
    ))
}

fn out_of_memory(m_cost_kib: u32) -> Error {
    Error::Internal(format!(
        "not enough memory for key derivation ({m_cost_kib} KiB)"
    ))
}

fn internal(message: &str) -> Error {
    Error::Internal(message.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(super) mod tests {
    use super::*;
    use crate::error::ErrorKind;

    /// Cheapest valid cost, so that tests do not spend seconds in Argon2.
    pub(crate) const TEST_COST: KdfCost = KdfCost {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    };

    const REFERENCE: &str = "5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11/secret/1";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn test_key(password: &str) -> (MasterKey, KdfParams) {
        let kdf = new_kdf_params(TEST_COST).unwrap();
        (derive_key(password, &kdf).unwrap(), kdf)
    }

    fn decode(envelope: &str) -> Vec<u8> {
        STANDARD.decode(envelope).unwrap()
    }

    fn encode(blob: &[u8]) -> String {
        STANDARD.encode(blob)
    }

    #[test]
    fn envelope_round_trip() {
        let (key, _) = test_key("correct horse");
        for value in [SECRET, "x", "p\u{e4}ssw\u{f6}rd with spaces and \u{1f511}"] {
            let envelope = seal_secret(&key, REFERENCE, value).unwrap();
            let opened = open_secret(&key, REFERENCE, &envelope).unwrap();
            assert_eq!(opened.expose(), value);
            if value.len() >= 8 {
                assert!(!envelope.contains(value));
            }
        }
    }

    #[test]
    fn every_encryption_uses_a_fresh_nonce() {
        let (key, _) = test_key("correct horse");
        let first = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        let second = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        assert_ne!(first[2..14], second[2..14]);
        assert_ne!(first, second);
    }

    #[test]
    fn envelope_layout() {
        let (key, _) = test_key("correct horse");
        let blob = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        assert_eq!(blob[0], MAGIC);
        assert_eq!(blob[1], VERSION);
        assert_eq!(blob.len(), HEADER_LEN + NONCE_LEN + SECRET.len() + TAG_LEN);
    }

    #[test]
    fn any_flipped_bit_is_detected() {
        let (key, _) = test_key("correct horse");
        let blob = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        for byte in HEADER_LEN..blob.len() {
            for bit in 0..8 {
                let mut tampered = blob.clone();
                tampered[byte] ^= 1 << bit;
                assert_eq!(
                    open_secret(&key, REFERENCE, &encode(&tampered)).err(),
                    Some(EnvelopeError::IntegrityCheckFailed),
                    "byte {byte} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn wrong_reference_fails() {
        let (key, _) = test_key("correct horse");
        let envelope = seal_secret(&key, REFERENCE, SECRET).unwrap();
        for other in [
            "5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11/secret/2",
            "5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11/session-token/1",
            "proxy/password/1",
        ] {
            assert_eq!(
                open_secret(&key, other, &envelope).err(),
                Some(EnvelopeError::IntegrityCheckFailed)
            );
        }
    }

    #[test]
    fn wrong_key_fails() {
        let (key, _) = test_key("correct horse");
        let (other, _) = test_key("battery staple");
        let envelope = seal_secret(&key, REFERENCE, SECRET).unwrap();
        assert_eq!(
            open_secret(&other, REFERENCE, &envelope).err(),
            Some(EnvelopeError::IntegrityCheckFailed)
        );
    }

    #[test]
    fn truncated_envelopes_fail() {
        let (key, _) = test_key("correct horse");
        let blob = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        for len in 0..HEADER_LEN + NONCE_LEN + TAG_LEN {
            let result = open_secret(&key, REFERENCE, &encode(&blob[..len])).err();
            assert_eq!(result, Some(EnvelopeError::Truncated), "length {len}");
        }
        // Long enough to parse but missing the end of the tag.
        for cut in 1..=TAG_LEN {
            let result = open_secret(&key, REFERENCE, &encode(&blob[..blob.len() - cut])).err();
            assert_eq!(
                result,
                Some(EnvelopeError::IntegrityCheckFailed),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn wrong_version_and_magic_fail() {
        let (key, _) = test_key("correct horse");
        let blob = decode(&seal_secret(&key, REFERENCE, SECRET).unwrap());
        let mut version_2 = blob.clone();
        version_2[1] = 2;
        assert_eq!(
            open_secret(&key, REFERENCE, &encode(&version_2)).err(),
            Some(EnvelopeError::UnsupportedVersion(2))
        );
        let mut no_magic = blob;
        no_magic[0] = 0x00;
        assert_eq!(
            open_secret(&key, REFERENCE, &encode(&no_magic)).err(),
            Some(EnvelopeError::NotAnEnvelope)
        );
    }

    #[test]
    fn non_base64_and_plaintext_fail() {
        let (key, _) = test_key("correct horse");
        assert_eq!(
            open_secret(&key, REFERENCE, "not base64!").err(),
            Some(EnvelopeError::NotBase64)
        );
        assert_eq!(
            open_secret(&key, REFERENCE, SECRET).err(),
            Some(EnvelopeError::NotAnEnvelope)
        );
        assert_eq!(
            open_secret(&key, REFERENCE, "").err(),
            Some(EnvelopeError::Truncated)
        );
    }

    #[test]
    fn verifier_accepts_only_the_right_password_and_parameters() {
        let (key, kdf) = test_key("correct horse");
        let config = SecurityConfig {
            verifier_b64: make_verifier(&key, &kdf).unwrap(),
            kdf,
        };
        check_security_config(&config).unwrap();
        assert!(verify(&key, &config).unwrap());

        let wrong = derive_key("correct horsE", &config.kdf).unwrap();
        assert!(!verify(&wrong, &config).unwrap());

        // Parameters are authenticated: a modified config does not verify.
        let mut modified = config.clone();
        modified.kdf.t_cost += 1;
        assert!(!verify(&key, &modified).unwrap());
        let mut modified = config.clone();
        modified.kdf.salt_b64 = STANDARD.encode([7u8; 16]);
        assert!(!verify(&key, &modified).unwrap());

        // A secret envelope is not a verifier, even under the same key.
        let mut swapped = config.clone();
        swapped.verifier_b64 = seal_secret(
            &key,
            "x/y/1",
            std::str::from_utf8(VERIFIER_PLAINTEXT).unwrap(),
        )
        .unwrap();
        assert!(!verify(&key, &swapped).unwrap());
    }

    #[test]
    fn derivation_is_deterministic_and_salted() {
        let kdf = new_kdf_params(TEST_COST).unwrap();
        let a = derive_key("correct horse", &kdf).unwrap();
        let b = derive_key("correct horse", &kdf).unwrap();
        assert_eq!(a.0.as_slice(), b.0.as_slice());
        let other_salt = new_kdf_params(TEST_COST).unwrap();
        assert_ne!(other_salt.salt_b64, kdf.salt_b64);
        let c = derive_key("correct horse", &other_salt).unwrap();
        assert_ne!(a.0.as_slice(), c.0.as_slice());
    }

    #[test]
    fn known_answer() {
        // Fixed salt and parameters: guards against accidental changes of the
        // derivation (algorithm, version, output length).
        let kdf = KdfParams {
            algorithm: KDF_ALGORITHM.to_owned(),
            m_cost_kib: 32,
            t_cost: 3,
            p_cost: 4,
            salt_b64: STANDARD.encode([2u8; 16]),
        };
        let key = derive_key("password", &kdf).unwrap();
        let expected = {
            let argon2 = Argon2::new(
                Algorithm::Argon2id,
                Version::V0x13,
                Params::new(32, 3, 4, Some(32)).unwrap(),
            );
            let mut out = [0u8; 32];
            argon2
                .hash_password_into(b"password", &[2u8; 16], &mut out)
                .unwrap();
            out
        };
        assert_eq!(key.0.as_slice(), expected.as_slice());
    }

    #[test]
    fn recommended_cost_is_valid() {
        check_cost(KdfCost::RECOMMENDED).unwrap();
        let kdf = new_kdf_params(KdfCost::default()).unwrap();
        assert_eq!(kdf.algorithm, "argon2id");
        assert_eq!((kdf.m_cost_kib, kdf.t_cost, kdf.p_cost), (262_144, 3, 4));
        assert_eq!(STANDARD.decode(&kdf.salt_b64).unwrap().len(), SALT_LEN);
    }

    #[test]
    fn hostile_parameters_are_rejected() {
        let base = new_kdf_params(TEST_COST).unwrap();
        let cases: Vec<fn(&mut KdfParams)> = vec![
            |k| k.algorithm = "argon2i".to_owned(),
            |k| k.algorithm = "pbkdf2".to_owned(),
            |k| k.m_cost_kib = MAX_M_COST_KIB + 1,
            |k| k.m_cost_kib = u32::MAX,
            |k| k.m_cost_kib = 7,
            |k| k.t_cost = 0,
            |k| k.t_cost = MAX_T_COST + 1,
            |k| k.p_cost = 0,
            |k| k.p_cost = MAX_P_COST + 1,
            |k| k.salt_b64 = "%%%".to_owned(),
            |k| k.salt_b64 = STANDARD.encode([1u8; 8]),
            |k| k.salt_b64 = STANDARD.encode([1u8; 65]),
        ];
        for (index, mutate) in cases.iter().enumerate() {
            let mut kdf = base.clone();
            mutate(&mut kdf);
            let error = derive_key("password", &kdf).err().map(|e| e.kind());
            assert_eq!(error, Some(ErrorKind::Usage), "case {index}");
        }
    }

    #[test]
    fn password_rules() {
        assert_eq!(
            check_password("").err().map(|e| e.kind()),
            Some(ErrorKind::Usage)
        );
        assert!(check_password(&"a".repeat(MAX_PASSWORD_LEN)).is_ok());
        assert_eq!(
            check_password(&"a".repeat(MAX_PASSWORD_LEN + 1))
                .err()
                .map(|e| e.kind()),
            Some(ErrorKind::Usage)
        );
    }

    #[test]
    fn malformed_verifier_is_rejected_structurally() {
        let (key, kdf) = test_key("correct horse");
        let good = SecurityConfig {
            verifier_b64: make_verifier(&key, &kdf).unwrap(),
            kdf,
        };
        let blob = decode(&good.verifier_b64);
        for verifier in [
            "not base64".to_owned(),
            String::new(),
            encode(&blob[..blob.len() - 1]),
            encode(&[&blob[..], &[0u8][..]].concat()),
            encode(&[&[MAGIC, 2][..], &blob[2..]].concat()),
        ] {
            let config = SecurityConfig {
                verifier_b64: verifier,
                kdf: good.kdf.clone(),
            };
            assert_eq!(
                check_security_config(&config).err().map(|e| e.kind()),
                Some(ErrorKind::Usage)
            );
        }
    }

    #[test]
    fn key_debug_is_redacted() {
        let (key, _) = test_key("correct horse");
        assert_eq!(format!("{key:?}"), "MasterKey(<redacted>)");
    }
}
