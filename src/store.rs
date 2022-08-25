use crate::onion::Onion;
use crate::secp::{self, Commitment, RangeProof, SecretKey};

use grin_core::core::Input;
use grin_core::ser::{self, ProtocolVersion, Readable, Reader, Writeable, Writer};
use grin_store::{self as store, Store};
use grin_util::ToHex;
use thiserror::Error;

const DB_NAME: &str = "swap";
const STORE_SUBPATH: &str = "swaps";

const CURRENT_VERSION: u8 = 0;
const SWAP_PREFIX: u8 = b'S';

/// Data needed to swap a single output.
#[derive(Clone, Debug, PartialEq)]
pub struct SwapData {
	/// The total excess for the output commitment
	pub excess: SecretKey,
	/// The derived output commitment after applying excess and fee
	pub output_commit: Commitment,
	/// The rangeproof, included only for the final hop (node N)
	pub rangeproof: Option<RangeProof>,
	/// Transaction input being spent
	pub input: Input,
	/// Transaction fee
	pub fee: u64,
	/// The remaining onion after peeling off our layer
	pub onion: Onion,
	// todo: include a SwapStatus enum value
}

impl Writeable for SwapData {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		writer.write_u8(CURRENT_VERSION)?;
		writer.write_fixed_bytes(&self.excess)?;
		writer.write_fixed_bytes(&self.output_commit)?;

		// todo: duplicated in payload. Can we impl Writeable for Option<RangeProof>?
		match &self.rangeproof {
			Some(proof) => {
				writer.write_u8(1)?;
				proof.write(writer)?;
			}
			None => writer.write_u8(0)?,
		};

		self.input.write(writer)?;
		writer.write_u64(self.fee.into())?;
		self.onion.write(writer)?;

		Ok(())
	}
}

impl Readable for SwapData {
	fn read<R: Reader>(reader: &mut R) -> Result<SwapData, ser::Error> {
		let version = reader.read_u8()?;
		if version != CURRENT_VERSION {
			return Err(ser::Error::UnsupportedProtocolVersion);
		}

		let excess = secp::read_secret_key(reader)?;
		let output_commit = Commitment::read(reader)?;
		let rangeproof = if reader.read_u8()? == 0 {
			None
		} else {
			Some(RangeProof::read(reader)?)
		};
		let input = Input::read(reader)?;
		let fee = reader.read_u64()?;
		let onion = Onion::read(reader)?;

		Ok(SwapData {
			excess,
			output_commit,
			rangeproof,
			input,
			fee,
			onion,
		})
	}
}

/// Storage facility for swap data.
pub struct SwapStore {
	db: Store,
}

/// Store error types
#[derive(Clone, Error, Debug, PartialEq)]
pub enum StoreError {
	#[error("Error occurred while attempting to open db: {0}")]
	OpenError(store::lmdb::Error),
	#[error("Serialization error occurred: {0}")]
	SerializationError(ser::Error),
	#[error("Error occurred while attempting to read from db: {0}")]
	ReadError(store::lmdb::Error),
	#[error("Error occurred while attempting to write to db: {0}")]
	WriteError(store::lmdb::Error),
}

impl From<ser::Error> for StoreError {
	fn from(e: ser::Error) -> StoreError {
		StoreError::SerializationError(e)
	}
}

impl SwapStore {
	/// Create new chain store
	#[allow(dead_code)]
	pub fn new(db_root: &str) -> Result<SwapStore, StoreError> {
		let db = Store::new(db_root, Some(DB_NAME), Some(STORE_SUBPATH), None)
			.map_err(StoreError::OpenError)?;
		Ok(SwapStore { db })
	}

	/// Writes a single key-value pair to the database
	fn write<K: AsRef<[u8]>>(
		&self,
		prefix: u8,
		k: K,
		value: &Vec<u8>,
	) -> Result<(), store::lmdb::Error> {
		let batch = self.db.batch()?;
		batch.put(&store::to_key(prefix, k)[..], &value[..])?;
		batch.commit()
	}

	/// Reads a single value by key
	fn read<K: AsRef<[u8]> + Copy, V: Readable>(&self, prefix: u8, k: K) -> Result<V, StoreError> {
		store::option_to_not_found(self.db.get_ser(&store::to_key(prefix, k)[..], None), || {
			format!("{}:{}", prefix, k.to_hex())
		})
		.map_err(StoreError::ReadError)
	}

	/// Saves a swap to the database
	#[allow(dead_code)]
	pub fn save_swap(&self, s: &SwapData) -> Result<(), StoreError> {
		let data = ser::ser_vec(&s, ProtocolVersion::local())?;
		self.write(SWAP_PREFIX, &s.output_commit, &data)
			.map_err(StoreError::WriteError)
	}

	/// Reads a swap from the database
	#[allow(dead_code)]
	pub fn get_swap(&self, commit: &Commitment) -> Result<SwapData, StoreError> {
		self.read(SWAP_PREFIX, commit)
	}
}
