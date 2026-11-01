//! The streaming envelope format (version 1) and its codec.
//!
//! ```text
//! header (57 bytes, big endian)
//!   magic "BBCSE" | version u8 | compression u8 | kdf u8 (1 = argon2id)
//!   m_cost_kib u32 | t_cost u32 | p_cost u32 | chunk_log2 u8
//!   salt [16] | nonce_prefix [4] | key_check [16]
//! body: chunks until end of stream
//!   ciphertext_len u32 | ciphertext (plaintext chunk + 16 byte tag)
//! ```
//!
//! * The key is Argon2id(password, salt, cost) and is unique to the file.
//! * `key_check` is the tag of AES-256-GCM over an empty message with the
//!   reserved nonce `prefix || u64::MAX` and the header (without the check)
//!   as associated data. It tells a wrong password from damaged data before
//!   a byte of the body is read.
//! * Chunk `i` is sealed with nonce `prefix || i` (u64) and the associated
//!   data `"BBCSE chunk" || header || i || final`, where `final` is 1 for the
//!   last chunk only. Reordering, dropping, repeating or cutting chunks, and
//!   changing any header field, therefore fails authentication.
//! * The last chunk is always present, possibly empty (an empty file is a
//!   header and one empty chunk). Every other chunk holds exactly
//!   `1 << chunk_log2` plaintext bytes.
//! * Compression (gzip or zstd) is applied to the whole plaintext stream
//!   before it is cut into chunks.
//!
//! Everything streams: memory use is one chunk plus the Argon2 working set.

use std::fmt;
use std::io::{self, Read, Write};

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::secrets::KdfCost;

/// First bytes of every envelope.
pub const MAGIC: &[u8; 5] = b"BBCSE";
/// The format version this build writes and reads.
pub const FORMAT_VERSION: u8 = 1;
/// Length of the header in bytes.
pub const HEADER_LEN: usize = 57;
/// Largest password, in bytes (the limit of the master password).
pub const MAX_PASSWORD_LEN: usize = 1024;
/// Smallest and largest `chunk_log2` (4 KiB to 64 MiB).
pub const CHUNK_LOG2_RANGE: std::ops::RangeInclusive<u8> = 12..=26;
/// The default chunk size exponent (1 MiB chunks).
pub const DEFAULT_CHUNK_LOG2: u8 = 20;

const KDF_ARGON2ID: u8 = 1;
const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const PREFIX_LEN: usize = 4;
const TAG_LEN: usize = 16;
const CHECK_OFFSET: usize = HEADER_LEN - TAG_LEN;
const MAX_M_COST_KIB: u32 = 1024 * 1024;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 16;
const COPY_BUF: usize = 256 * 1024;

/// The Argon2id cost new envelopes use: 64 MiB, 3 passes, 4 lanes (the RFC
/// 9106 option for constrained environments, about 85 ms), so that folders of
/// files and several parallel tasks stay affordable.
pub const DEFAULT_KDF_COST: KdfCost = KdfCost {
    m_cost_kib: 64 * 1024,
    t_cost: 3,
    p_cost: 4,
};

/// Compression applied before encryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    /// No compression.
    #[default]
    None,
    /// gzip (DEFLATE).
    Gzip,
    /// Zstandard.
    Zstd,
}

impl Compression {
    /// The wire and command line spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Gzip => "gzip",
            Compression::Zstd => "zstd",
        }
    }

    fn code(self) -> u8 {
        match self {
            Compression::None => 0,
            Compression::Gzip => 1,
            Compression::Zstd => 2,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Compression::None),
            1 => Some(Compression::Gzip),
            2 => Some(Compression::Zstd),
            _ => None,
        }
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Compression {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "none" => Ok(Compression::None),
            "gzip" => Ok(Compression::Gzip),
            "zstd" => Ok(Compression::Zstd),
            other => Err(Error::Usage(format!(
                "'{other}' is not a compression; use none, gzip or zstd"
            ))),
        }
    }
}

/// How to encrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptOptions {
    /// Compression before encryption.
    pub compression: Compression,
    /// Argon2id cost of the key derivation.
    pub kdf: KdfCost,
    /// Plaintext chunk size is `1 << chunk_log2` ([`CHUNK_LOG2_RANGE`]).
    pub chunk_log2: u8,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        EncryptOptions {
            compression: Compression::None,
            kdf: DEFAULT_KDF_COST,
            chunk_log2: DEFAULT_CHUNK_LOG2,
        }
    }
}

/// The public fields of an envelope header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HeaderInfo {
    /// Format version.
    pub version: u8,
    /// Compression inside the envelope.
    pub compression: Compression,
    /// Key derivation cost.
    #[serde(skip)]
    pub kdf: KdfCost,
    /// Plaintext chunk size in bytes.
    pub chunk_size: u32,
}

/// What an operation processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Bytes read from the input.
    pub bytes_in: u64,
    /// Bytes written to the output.
    pub bytes_out: u64,
    /// The header of the envelope that was written or read.
    pub header: HeaderInfo,
}

/// A failure of the format that the caller can tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The authenticated body does not verify: truncated, reordered, or
    /// modified.
    Tampered,
    /// The caller stopped the operation.
    Stopped,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Tampered => f.write_str(
                "the encrypted data failed its integrity check (truncated, reordered or modified)",
            ),
            Failure::Stopped => f.write_str("operation cancelled"),
        }
    }
}

impl std::error::Error for Failure {}

fn failure(kind: Failure) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, kind)
}

/// Turns an I/O error of the codec back into an [`Error`].
fn from_io(err: io::Error, context: &str) -> Error {
    match err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Failure>())
    {
        Some(Failure::Stopped) => Error::Cancelled,
        _ => Error::io(context, err),
    }
}

/// Whether `err` is an integrity failure of the encrypted body.
pub(crate) fn is_tampered(err: &Error) -> bool {
    matches!(err, Error::Io { source, .. }
        if source.get_ref().and_then(|inner| inner.downcast_ref::<Failure>()) == Some(&Failure::Tampered))
}

/// A parsed header with its raw bytes.
struct Header {
    raw: [u8; HEADER_LEN],
    info: HeaderInfo,
    chunk_log2: u8,
    salt: [u8; SALT_LEN],
    prefix: [u8; PREFIX_LEN],
}

impl Header {
    fn parse(raw: [u8; HEADER_LEN]) -> Result<Header> {
        if &raw[..5] != MAGIC {
            return Err(not_encrypted());
        }
        let version = raw[5];
        if version != FORMAT_VERSION {
            return Err(Error::Usage(format!(
                "unsupported client-side encryption format version {version} (this build reads version {FORMAT_VERSION})"
            )));
        }
        let corrupt = |what: &str| Error::Usage(format!("damaged encryption header: {what}"));
        let compression =
            Compression::from_code(raw[6]).ok_or_else(|| corrupt("unknown compression"))?;
        if raw[7] != KDF_ARGON2ID {
            return Err(corrupt("unknown key derivation"));
        }
        let word = |at: usize| u32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let kdf = KdfCost {
            m_cost_kib: word(8),
            t_cost: word(12),
            p_cost: word(16),
        };
        argon_params(kdf).map_err(|_| corrupt("key derivation cost out of range"))?;
        let chunk_log2 = raw[20];
        if !CHUNK_LOG2_RANGE.contains(&chunk_log2) {
            return Err(corrupt("chunk size out of range"));
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&raw[21..21 + SALT_LEN]);
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&raw[37..37 + PREFIX_LEN]);
        Ok(Header {
            raw,
            info: HeaderInfo {
                version,
                compression,
                kdf,
                chunk_size: 1u32 << chunk_log2,
            },
            chunk_log2,
            salt,
            prefix,
        })
    }

    fn chunk_size(&self) -> usize {
        1usize << self.chunk_log2
    }
}

fn not_encrypted() -> Error {
    Error::Usage("the data is not client-side encrypted by BucketBridge".to_owned())
}

/// Error text of the two password states; the UI and the CLI tell them apart
/// by [`crate::cse::Problem`].
pub(crate) const PASSWORD_NOT_FOUND: &str =
    "password not found: the data is encrypted and no password was given";
pub(crate) const PASSWORD_NOT_VALID: &str = "password not valid: it does not open the data";

fn argon_params(cost: KdfCost) -> Result<Params> {
    if !(1..=MAX_P_COST).contains(&cost.p_cost)
        || !(1..=MAX_T_COST).contains(&cost.t_cost)
        || cost.m_cost_kib < 8 * cost.p_cost
        || cost.m_cost_kib > MAX_M_COST_KIB
    {
        return Err(Error::Usage(
            "the key derivation cost is out of range".to_owned(),
        ));
    }
    Params::new(cost.m_cost_kib, cost.t_cost, cost.p_cost, Some(KEY_LEN))
        .map_err(|error| Error::Usage(format!("invalid Argon2 parameters: {error}")))
}

/// Argon2id of `password` (blocks for the cost of `kdf`).
fn derive_key(password: &str, kdf: KdfCost, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    if password.is_empty() {
        return Err(Error::Usage("the password must not be empty".to_owned()));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(Error::Usage(format!(
            "the password must not be longer than {MAX_PASSWORD_LEN} bytes"
        )));
    }
    let params = argon_params(kdf)?;
    let block_count = params.block_count();
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut blocks: Vec<Block> = Vec::new();
    blocks.try_reserve_exact(block_count).map_err(|_| {
        Error::Internal(format!(
            "not enough memory for key derivation ({} KiB)",
            kdf.m_cost_kib
        ))
    })?;
    blocks.resize(block_count, Block::new());
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let result = argon2.hash_password_into_with_memory(
        password.as_bytes(),
        salt,
        key.as_mut_slice(),
        &mut blocks,
    );
    blocks.zeroize();
    result.map_err(|error| Error::Internal(format!("key derivation failed: {error}")))?;
    Ok(key)
}

fn cipher(key: &[u8; KEY_LEN]) -> Result<Aes256Gcm> {
    Aes256Gcm::new_from_slice(key).map_err(|_| Error::Internal("invalid key length".to_owned()))
}

fn nonce(prefix: &[u8; PREFIX_LEN], index: u64) -> Nonce<Aes256Gcm> {
    let mut bytes = [0u8; 12];
    bytes[..PREFIX_LEN].copy_from_slice(prefix);
    bytes[PREFIX_LEN..].copy_from_slice(&index.to_be_bytes());
    Nonce::<Aes256Gcm>::from(bytes)
}

fn check_aad(head: &[u8]) -> Vec<u8> {
    let mut aad = b"BBCSE check".to_vec();
    aad.extend_from_slice(head);
    aad
}

fn chunk_aad(header: &[u8; HEADER_LEN], index: u64, last: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(11 + HEADER_LEN + 9);
    aad.extend_from_slice(b"BBCSE chunk");
    aad.extend_from_slice(header);
    aad.extend_from_slice(&index.to_be_bytes());
    aad.push(u8::from(last));
    aad
}

fn random(buffer: &mut [u8]) -> Result<()> {
    getrandom::fill(buffer)
        .map_err(|error| Error::Internal(format!("the OS random number generator failed: {error}")))
}

// ---- writing ----

struct SealWriter<W: Write> {
    out: W,
    cipher: Aes256Gcm,
    header: [u8; HEADER_LEN],
    prefix: [u8; PREFIX_LEN],
    chunk_size: usize,
    buf: Vec<u8>,
    index: u64,
    written: u64,
}

impl<W: Write> SealWriter<W> {
    fn seal(&mut self, last: bool) -> io::Result<()> {
        let sealed = self
            .cipher
            .encrypt(
                &nonce(&self.prefix, self.index),
                Payload {
                    msg: &self.buf,
                    aad: &chunk_aad(&self.header, self.index, last),
                },
            )
            .map_err(|_| io::Error::other("encryption failed"))?;
        let len = u32::try_from(sealed.len()).map_err(|_| io::Error::other("chunk too large"))?;
        self.out.write_all(&len.to_be_bytes())?;
        self.out.write_all(&sealed)?;
        self.written += 4 + sealed.len() as u64;
        self.index += 1;
        self.buf.clear();
        Ok(())
    }

    /// Writes the last chunk and returns the output and the bytes written
    /// after the header.
    fn finish(mut self) -> io::Result<(W, u64)> {
        self.seal(true)?;
        self.out.flush()?;
        Ok((self.out, self.written))
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.buf.len() == self.chunk_size {
            self.seal(false)?;
        }
        let take = data.len().min(self.chunk_size - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads from `input`, reporting progress and honouring the stop request.
struct Metered<'a, R> {
    inner: R,
    count: u64,
    progress: &'a mut dyn FnMut(u64) -> bool,
}

impl<R: Read> Read for Metered<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        if !(self.progress)(self.count) {
            return Err(failure(Failure::Stopped));
        }
        Ok(n)
    }
}

/// Counts what is written.
struct CountWrite<W> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for CountWrite<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(data)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn copy<R: Read, W: Write + ?Sized>(input: &mut R, output: &mut W) -> io::Result<()> {
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        output.write_all(&buf[..n])?;
    }
}

/// Encrypts `input` into `output` (blocking; run it on a blocking thread).
///
/// `progress` receives the bytes of input read so far and returns `false` to
/// stop (the call then fails with [`Error::Cancelled`]).
pub fn encrypt<R: Read, W: Write>(
    input: R,
    mut output: W,
    password: &str,
    options: &EncryptOptions,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<Summary> {
    if !CHUNK_LOG2_RANGE.contains(&options.chunk_log2) {
        return Err(Error::Usage(format!(
            "the chunk size exponent must be between {} and {}",
            CHUNK_LOG2_RANGE.start(),
            CHUNK_LOG2_RANGE.end()
        )));
    }
    argon_params(options.kdf)?;
    let mut raw = [0u8; HEADER_LEN];
    raw[..5].copy_from_slice(MAGIC);
    raw[5] = FORMAT_VERSION;
    raw[6] = options.compression.code();
    raw[7] = KDF_ARGON2ID;
    raw[8..12].copy_from_slice(&options.kdf.m_cost_kib.to_be_bytes());
    raw[12..16].copy_from_slice(&options.kdf.t_cost.to_be_bytes());
    raw[16..20].copy_from_slice(&options.kdf.p_cost.to_be_bytes());
    raw[20] = options.chunk_log2;
    random(&mut raw[21..21 + SALT_LEN])?;
    random(&mut raw[37..37 + PREFIX_LEN])?;
    let mut prefix = [0u8; PREFIX_LEN];
    prefix.copy_from_slice(&raw[37..37 + PREFIX_LEN]);
    let key = derive_key(password, options.kdf, &raw[21..21 + SALT_LEN])?;
    let cipher = cipher(&key)?;
    let check = cipher
        .encrypt(
            &nonce(&prefix, u64::MAX),
            Payload {
                msg: &[],
                aad: &check_aad(&raw[..CHECK_OFFSET]),
            },
        )
        .map_err(|_| Error::Internal("encryption failed".to_owned()))?;
    raw[CHECK_OFFSET..].copy_from_slice(&check);
    let header = Header::parse(raw)?;
    let context = "encrypting";
    output
        .write_all(&raw)
        .map_err(|err| Error::io(context, err))?;
    let sealer = SealWriter {
        out: output,
        cipher,
        header: raw,
        prefix,
        chunk_size: header.chunk_size(),
        buf: Vec::with_capacity(header.chunk_size()),
        index: 0,
        written: 0,
    };
    let mut metered = Metered {
        inner: input,
        count: 0,
        progress,
    };
    let sealed = match options.compression {
        Compression::None => {
            let mut sealer = sealer;
            copy(&mut metered, &mut sealer).map_err(|err| from_io(err, context))?;
            sealer
        }
        Compression::Gzip => {
            let mut encoder = flate2::write::GzEncoder::new(sealer, flate2::Compression::default());
            copy(&mut metered, &mut encoder).map_err(|err| from_io(err, context))?;
            encoder.finish().map_err(|err| from_io(err, context))?
        }
        Compression::Zstd => {
            let mut encoder = zstd::stream::write::Encoder::new(sealer, 3)
                .map_err(|err| from_io(err, context))?;
            copy(&mut metered, &mut encoder).map_err(|err| from_io(err, context))?;
            encoder.finish().map_err(|err| from_io(err, context))?
        }
    };
    let (_, body) = sealed.finish().map_err(|err| from_io(err, context))?;
    Ok(Summary {
        bytes_in: metered.count,
        bytes_out: HEADER_LEN as u64 + body,
        header: header.info,
    })
}

// ---- reading ----

struct OpenReader<R: Read> {
    input: R,
    cipher: Aes256Gcm,
    header: [u8; HEADER_LEN],
    prefix: [u8; PREFIX_LEN],
    chunk_size: usize,
    plain: Vec<u8>,
    at: usize,
    index: u64,
    next_len: Option<u32>,
    started: bool,
    done: bool,
}

impl<R: Read> OpenReader<R> {
    /// Reads a 4 byte length; `None` at a clean end of input.
    fn read_len(&mut self) -> io::Result<Option<u32>> {
        let mut bytes = [0u8; 4];
        let mut filled = 0;
        while filled < 4 {
            match self.input.read(&mut bytes[filled..]) {
                Ok(0) if filled == 0 => return Ok(None),
                Ok(0) => return Err(failure(Failure::Tampered)),
                Ok(n) => filled += n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(Some(u32::from_be_bytes(bytes)))
    }

    /// Decrypts the next chunk into `plain`; `false` when there is none.
    fn next_chunk(&mut self) -> io::Result<bool> {
        if self.done {
            return Ok(false);
        }
        let len = if self.started {
            self.next_len.take()
        } else {
            self.started = true;
            self.read_len()?
        };
        let Some(len) = len else {
            // No chunk at all, or the stream ended after a non-final chunk.
            return Err(failure(Failure::Tampered));
        };
        let len = len as usize;
        if len < TAG_LEN || len > self.chunk_size + TAG_LEN {
            return Err(failure(Failure::Tampered));
        }
        let mut sealed = vec![0u8; len];
        self.input.read_exact(&mut sealed).map_err(|err| {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                failure(Failure::Tampered)
            } else {
                err
            }
        })?;
        self.next_len = self.read_len()?;
        let last = self.next_len.is_none();
        let plain = self
            .cipher
            .decrypt(
                &nonce(&self.prefix, self.index),
                Payload {
                    msg: &sealed,
                    aad: &chunk_aad(&self.header, self.index, last),
                },
            )
            .map_err(|_| failure(Failure::Tampered))?;
        if !last && plain.len() != self.chunk_size {
            return Err(failure(Failure::Tampered));
        }
        self.index += 1;
        self.done = last;
        self.plain.zeroize();
        self.plain = plain;
        self.at = 0;
        Ok(true)
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            if !self.next_chunk()? {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.plain.len() - self.at);
        buf[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl<R: Read> Drop for OpenReader<R> {
    fn drop(&mut self) {
        self.plain.zeroize();
    }
}

fn read_header_bytes<R: Read>(input: &mut R) -> Result<[u8; HEADER_LEN]> {
    let mut raw = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match input.read(&mut raw[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(from_io(err, "reading the encryption header")),
        }
    }
    if filled < MAGIC.len() || &raw[..MAGIC.len()] != MAGIC {
        return Err(not_encrypted());
    }
    if filled < HEADER_LEN {
        return Err(Error::Usage(
            "damaged encryption header: the data is shorter than a header".to_owned(),
        ));
    }
    Ok(raw)
}

/// Reads the header of an envelope without a password.
///
/// Fails with [`Error::Usage`] when `input` does not start with one.
pub fn read_header<R: Read>(mut input: R) -> Result<HeaderInfo> {
    Ok(Header::parse(read_header_bytes(&mut input)?)?.info)
}

/// Parses the header at the start of `bytes`: `Ok(None)` when they do not
/// start an envelope, `Err` when they start a damaged or unsupported one.
pub fn inspect(bytes: &[u8]) -> Result<Option<HeaderInfo>> {
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }
    read_header(bytes).map(Some)
}

/// Checks `password` against the header only (one Argon2 run), without
/// reading the body. Fails with the "password not found" state for an empty
/// password and "password not valid" for a wrong one.
pub fn check_password(header: &[u8], password: Option<&str>) -> Result<HeaderInfo> {
    let mut input = header;
    let raw = read_header_bytes(&mut input)?;
    let parsed = Header::parse(raw)?;
    open_key(&parsed, password)?;
    Ok(parsed.info)
}

fn open_key(header: &Header, password: Option<&str>) -> Result<Aes256Gcm> {
    let Some(password) = password.filter(|text| !text.is_empty()) else {
        return Err(Error::Auth(PASSWORD_NOT_FOUND.to_owned()));
    };
    let key = derive_key(password, header.info.kdf, &header.salt)?;
    let cipher = cipher(&key)?;
    let valid = cipher
        .decrypt(
            &nonce(&header.prefix, u64::MAX),
            Payload {
                msg: &header.raw[CHECK_OFFSET..],
                aad: &check_aad(&header.raw[..CHECK_OFFSET]),
            },
        )
        .is_ok();
    if valid {
        Ok(cipher)
    } else {
        Err(Error::Auth(PASSWORD_NOT_VALID.to_owned()))
    }
}

/// Decrypts `input` into `output` (blocking; run it on a blocking thread).
///
/// `password: None` fails with the "password not found" state, a wrong one
/// with "password not valid", before anything is written. `progress` receives
/// the bytes of input read so far and returns `false` to stop.
pub fn decrypt<R: Read, W: Write>(
    mut input: R,
    output: W,
    password: Option<&str>,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<Summary> {
    let raw = read_header_bytes(&mut input)?;
    let header = Header::parse(raw)?;
    let cipher = open_key(&header, password)?;
    let context = "decrypting";
    let metered = Metered {
        inner: input,
        count: HEADER_LEN as u64,
        progress,
    };
    let mut reader = OpenReader {
        input: metered,
        cipher,
        header: raw,
        prefix: header.prefix,
        chunk_size: header.chunk_size(),
        plain: Vec::new(),
        at: 0,
        index: 0,
        next_len: None,
        started: false,
        done: false,
    };
    let mut out = CountWrite {
        inner: output,
        count: 0,
    };
    match header.info.compression {
        Compression::None => copy(&mut reader, &mut out).map_err(|err| from_io(err, context))?,
        Compression::Gzip => {
            let mut decoder = flate2::read::GzDecoder::new(&mut reader);
            copy(&mut decoder, &mut out).map_err(|err| from_io(err, context))?;
        }
        Compression::Zstd => {
            let mut decoder = zstd::stream::read::Decoder::new(&mut reader)
                .map_err(|err| from_io(err, context))?;
            copy(&mut decoder, &mut out).map_err(|err| from_io(err, context))?;
        }
    }
    // The decompressor may stop before the end of the body: authenticate the
    // rest, so that a cut or extended body never goes unnoticed.
    copy(&mut reader, &mut io::sink()).map_err(|err| from_io(err, context))?;
    out.flush().map_err(|err| Error::io(context, err))?;
    Ok(Summary {
        bytes_in: reader.input.count,
        bytes_out: out.count,
        header: header.info,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cost low enough for tests.
    const FAST: KdfCost = KdfCost {
        m_cost_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    const PASSWORD: &str = "correct horse battery staple";

    fn options(compression: Compression) -> EncryptOptions {
        EncryptOptions {
            compression,
            kdf: FAST,
            chunk_log2: 12,
        }
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + i / 7) % 251) as u8).collect()
    }

    fn seal(data: &[u8], compression: Compression) -> Vec<u8> {
        let mut out = Vec::new();
        encrypt(data, &mut out, PASSWORD, &options(compression), &mut |_| {
            true
        })
        .unwrap();
        out
    }

    fn open(blob: &[u8], password: Option<&str>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        decrypt(blob, &mut out, password, &mut |_| true).map(|_| out)
    }

    const ALL: [Compression; 3] = [Compression::None, Compression::Gzip, Compression::Zstd];

    #[test]
    fn round_trips_every_size_and_compression() {
        let chunk = 1usize << 12;
        for compression in ALL {
            for len in [
                0,
                1,
                chunk - 1,
                chunk,
                chunk + 1,
                2 * chunk,
                5 * chunk + 17,
                40 * chunk,
            ] {
                let data = sample(len);
                let blob = seal(&data, compression);
                assert_eq!(
                    open(&blob, Some(PASSWORD)).unwrap(),
                    data,
                    "{compression} {len}"
                );
            }
        }
    }

    #[test]
    fn empty_input_is_a_header_and_one_final_chunk() {
        let blob = seal(&[], Compression::None);
        assert_eq!(blob.len(), HEADER_LEN + 4 + TAG_LEN);
        assert_eq!(&blob[..5], MAGIC);
    }

    #[test]
    fn compression_shrinks_compressible_data() {
        let data = vec![b'a'; 200_000];
        let plain = seal(&data, Compression::None).len();
        assert!(seal(&data, Compression::Gzip).len() < plain / 10);
        assert!(seal(&data, Compression::Zstd).len() < plain / 10);
    }

    #[test]
    fn two_encryptions_differ() {
        let data = sample(100);
        assert_ne!(
            seal(&data, Compression::None),
            seal(&data, Compression::None)
        );
    }

    #[test]
    fn header_is_readable_without_a_password() {
        let blob = seal(&sample(10), Compression::Zstd);
        let info = read_header(&blob[..]).unwrap();
        assert_eq!(info.compression, Compression::Zstd);
        assert_eq!(info.version, 1);
        assert_eq!(info.chunk_size, 4096);
        assert_eq!(inspect(&blob).unwrap(), Some(info));
        assert_eq!(inspect(b"plain text").unwrap(), None);
        assert_eq!(inspect(b"BB").unwrap(), None);
    }

    #[test]
    fn password_states_are_distinct() {
        let blob = seal(&sample(10), Compression::None);
        let missing = open(&blob, None).unwrap_err();
        assert!(missing.to_string().starts_with("password not found"));
        let empty = open(&blob, Some("")).unwrap_err();
        assert!(empty.to_string().starts_with("password not found"));
        let wrong = open(&blob, Some("wrong")).unwrap_err();
        assert!(wrong.to_string().starts_with("password not valid"));
        assert_eq!(wrong.kind(), crate::error::ErrorKind::Auth);
        assert!(!is_tampered(&wrong));
        assert!(check_password(&blob[..HEADER_LEN], Some(PASSWORD)).is_ok());
        assert!(check_password(&blob[..HEADER_LEN], Some("nope")).is_err());
    }

    #[test]
    fn wrong_password_writes_nothing() {
        let blob = seal(&sample(10), Compression::None);
        let mut out = Vec::new();
        assert!(decrypt(&blob[..], &mut out, Some("bad"), &mut |_| true).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn not_encrypted_data_is_a_usage_error() {
        let err = open(
            b"hello world, this is not an envelope at all, no.....",
            Some(PASSWORD),
        )
        .unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Usage);
        assert!(open(b"", Some(PASSWORD)).is_err());
    }

    #[test]
    fn flipping_any_body_byte_fails() {
        let blob = seal(&sample(3 * 4096 + 5), Compression::None);
        for at in [
            HEADER_LEN + 6,
            HEADER_LEN + 4100,
            blob.len() - 1,
            blob.len() - 20,
        ] {
            let mut bad = blob.clone();
            bad[at] ^= 0x01;
            let err = open(&bad, Some(PASSWORD)).unwrap_err();
            assert!(is_tampered(&err), "byte {at}: {err}");
        }
    }

    #[test]
    fn header_tampering_fails() {
        let blob = seal(&sample(50), Compression::Gzip);
        for at in [6, 20, 25, 40, CHECK_OFFSET + 2] {
            let mut bad = blob.clone();
            bad[at] ^= 0x01;
            assert!(open(&bad, Some(PASSWORD)).is_err(), "byte {at}");
        }
    }

    #[test]
    fn truncation_fails_everywhere() {
        let data = sample(3 * 4096 + 100);
        let blob = seal(&data, Compression::None);
        let first_chunk_end = HEADER_LEN + 4 + 4096 + TAG_LEN;
        // Cut at a chunk boundary, inside a chunk and inside a length prefix.
        for cut in [
            first_chunk_end,
            first_chunk_end + 2,
            first_chunk_end + 10,
            blob.len() - 1,
            HEADER_LEN,
            HEADER_LEN + 2,
        ] {
            let err = open(&blob[..cut], Some(PASSWORD)).unwrap_err();
            assert!(is_tampered(&err), "cut {cut}: {err}");
        }
    }

    #[test]
    fn truncation_after_compression_is_still_detected() {
        let data = vec![7u8; 20 * 4096];
        for compression in [Compression::Gzip, Compression::Zstd] {
            let blob = seal(&sample(40 * 4096), compression);
            let cut = HEADER_LEN + 4 + 4096 + TAG_LEN;
            assert!(open(&blob[..cut.min(blob.len() - 1)], Some(PASSWORD)).is_err());
            let _ = data.len();
        }
    }

    #[test]
    fn appended_data_fails() {
        let mut blob = seal(&sample(2 * 4096), Compression::None);
        blob.extend_from_slice(&[0, 0, 0, 20]);
        blob.extend_from_slice(&[1u8; 20]);
        assert!(open(&blob, Some(PASSWORD)).is_err());
        let mut blob = seal(&sample(10), Compression::None);
        blob.push(9);
        assert!(open(&blob, Some(PASSWORD)).is_err());
    }

    #[test]
    fn reordered_and_repeated_chunks_fail() {
        let blob = seal(&sample(4 * 4096 + 10), Compression::None);
        let step = 4 + 4096 + TAG_LEN;
        let chunk = |n: usize| &blob[HEADER_LEN + n * step..HEADER_LEN + (n + 1) * step];
        let last = &blob[HEADER_LEN + 4 * step..];
        let build = |order: &[&[u8]]| {
            let mut out = blob[..HEADER_LEN].to_vec();
            for part in order {
                out.extend_from_slice(part);
            }
            out
        };
        let swapped = build(&[chunk(1), chunk(0), chunk(2), chunk(3), last]);
        assert!(is_tampered(&open(&swapped, Some(PASSWORD)).unwrap_err()));
        let repeated = build(&[chunk(0), chunk(0), chunk(1), chunk(2), chunk(3), last]);
        assert!(is_tampered(&open(&repeated, Some(PASSWORD)).unwrap_err()));
        let dropped = build(&[chunk(0), chunk(2), chunk(3), last]);
        assert!(is_tampered(&open(&dropped, Some(PASSWORD)).unwrap_err()));
        let no_last = build(&[chunk(0), chunk(1), chunk(2), chunk(3)]);
        assert!(is_tampered(&open(&no_last, Some(PASSWORD)).unwrap_err()));
        let intact = build(&[chunk(0), chunk(1), chunk(2), chunk(3), last]);
        assert_eq!(open(&intact, Some(PASSWORD)).unwrap().len(), 4 * 4096 + 10);
    }

    #[test]
    fn chunks_cannot_move_between_files() {
        let one = seal(&sample(2 * 4096), Compression::None);
        let two = seal(&sample(2 * 4096), Compression::None);
        let mut mixed = one[..HEADER_LEN].to_vec();
        mixed.extend_from_slice(&two[HEADER_LEN..]);
        assert!(open(&mixed, Some(PASSWORD)).is_err());
    }

    #[test]
    fn oversized_chunk_length_is_rejected_without_allocation() {
        let mut blob = seal(&sample(10), Compression::None);
        blob[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(is_tampered(&open(&blob, Some(PASSWORD)).unwrap_err()));
    }

    #[test]
    fn progress_counts_input_and_can_stop() {
        let data = sample(10 * 4096);
        let mut seen = 0;
        let mut out = Vec::new();
        encrypt(
            &data[..],
            &mut out,
            PASSWORD,
            &options(Compression::None),
            &mut |n| {
                seen = n;
                true
            },
        )
        .unwrap();
        assert_eq!(seen, data.len() as u64);
        let err = encrypt(
            &data[..],
            Vec::new(),
            PASSWORD,
            &options(Compression::None),
            &mut |n| n < 4096,
        )
        .unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Cancelled);
        let err = decrypt(&out[..], Vec::new(), Some(PASSWORD), &mut |n| n < 10_000).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Cancelled);
    }

    #[test]
    fn summary_reports_sizes() {
        let data = sample(5000);
        let mut out = Vec::new();
        let summary = encrypt(
            &data[..],
            &mut out,
            PASSWORD,
            &options(Compression::None),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(summary.bytes_in, 5000);
        assert_eq!(summary.bytes_out, out.len() as u64);
        let mut plain = Vec::new();
        let back = decrypt(&out[..], &mut plain, Some(PASSWORD), &mut |_| true).unwrap();
        assert_eq!(back.bytes_out, 5000);
        assert_eq!(back.bytes_in, out.len() as u64);
    }

    #[test]
    fn option_and_password_validation() {
        let bad_chunk = EncryptOptions {
            chunk_log2: 3,
            ..options(Compression::None)
        };
        assert!(encrypt(&b""[..], Vec::new(), PASSWORD, &bad_chunk, &mut |_| true).is_err());
        let bad_kdf = EncryptOptions {
            kdf: KdfCost {
                m_cost_kib: 1,
                t_cost: 1,
                p_cost: 1,
            },
            ..options(Compression::None)
        };
        assert!(encrypt(&b""[..], Vec::new(), PASSWORD, &bad_kdf, &mut |_| true).is_err());
        assert!(
            encrypt(
                &b""[..],
                Vec::new(),
                "",
                &options(Compression::None),
                &mut |_| true
            )
            .is_err()
        );
        let long = "x".repeat(MAX_PASSWORD_LEN + 1);
        assert!(
            encrypt(
                &b""[..],
                Vec::new(),
                &long,
                &options(Compression::None),
                &mut |_| true
            )
            .is_err()
        );
    }

    #[test]
    fn compression_spellings() {
        for compression in ALL {
            assert_eq!(
                compression.as_str().parse::<Compression>().unwrap(),
                compression
            );
            assert_eq!(
                Compression::from_code(compression.code()),
                Some(compression)
            );
        }
        assert!("lz4".parse::<Compression>().is_err());
        assert_eq!(Compression::from_code(9), None);
    }
}
