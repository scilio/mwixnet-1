use crate::onion::Onion;
use crate::secp::{self, Commitment, RangeProof, SecretKey};
use crate::types::{read_optional, write_optional};
use grin_core::core::hash::Hash;

use grin_core::core::Input;
use grin_core::ser::{
	self, DeserializationMode, ProtocolVersion, Readable, Reader, Writeable, Writer,
};
use grin_store::{self as store, Store};
use grin_util::ToHex;
use thiserror::Error;

const DB_NAME: &str = "swap";
const STORE_SUBPATH: &str = "swaps";

const CURRENT_VERSION: u8 = 0;
const SWAP_PREFIX: u8 = b'S';

/// Swap statuses
#[derive(Clone, Debug, PartialEq)]
pub enum SwapStatus {
	Unprocessed,
	InProcess { kernel_hash: Hash },
	Completed { kernel_hash: Hash, block_hash: Hash },
}

impl Writeable for SwapStatus {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		match self {
			SwapStatus::Unprocessed => {
				writer.write_u8(0)?;
			}
			SwapStatus::InProcess { kernel_hash } => {
				writer.write_u8(1)?;
				kernel_hash.write(writer)?;
			}
			SwapStatus::Completed {
				kernel_hash,
				block_hash,
			} => {
				writer.write_u8(2)?;
				kernel_hash.write(writer)?;
				block_hash.write(writer)?;
			}
		};

		Ok(())
	}
}

impl Readable for SwapStatus {
	fn read<R: Reader>(reader: &mut R) -> Result<SwapStatus, ser::Error> {
		let status = match reader.read_u8()? {
			0 => SwapStatus::Unprocessed,
			1 => {
				let kernel_hash = Hash::read(reader)?;
				SwapStatus::InProcess { kernel_hash }
			}
			2 => {
				let kernel_hash = Hash::read(reader)?;
				let block_hash = Hash::read(reader)?;
				SwapStatus::Completed {
					kernel_hash,
					block_hash,
				}
			}
			_ => {
				return Err(ser::Error::CorruptedData);
			}
		};
		Ok(status)
	}
}

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
	/// The status of the swap
	pub status: SwapStatus,
}

impl Writeable for SwapData {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		writer.write_u8(CURRENT_VERSION)?;
		writer.write_fixed_bytes(&self.excess)?;
		writer.write_fixed_bytes(&self.output_commit)?;
		write_optional(writer, &self.rangeproof)?;
		self.input.write(writer)?;
		writer.write_u64(self.fee.into())?;
		self.onion.write(writer)?;
		self.status.write(writer)?;

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
		let rangeproof = read_optional(reader)?;
		let input = Input::read(reader)?;
		let fee = reader.read_u64()?;
		let onion = Onion::read(reader)?;
		let status = SwapStatus::read(reader)?;
		Ok(SwapData {
			excess,
			output_commit,
			rangeproof,
			input,
			fee,
			onion,
			status,
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
	#[error("Swap entry already exists for '{0:?}'")]
	AlreadyExists(Commitment),
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
		overwrite: bool,
	) -> Result<bool, store::lmdb::Error> {
		let batch = self.db.batch()?;
		let key = store::to_key(prefix, k);
		if !overwrite && batch.exists(&key[..])? {
			Ok(false)
		} else {
			batch.put(&key[..], &value[..])?;
			batch.commit()?;
			Ok(true)
		}
	}

	/// Reads a single value by key
	fn read<K: AsRef<[u8]> + Copy, V: Readable>(&self, prefix: u8, k: K) -> Result<V, StoreError> {
		store::option_to_not_found(self.db.get_ser(&store::to_key(prefix, k)[..], None), || {
			format!("{}:{}", prefix, k.to_hex())
		})
		.map_err(StoreError::ReadError)
	}

	/// Saves a swap to the database
	pub fn save_swap(&self, s: &SwapData, overwrite: bool) -> Result<(), StoreError> {
		let data = ser::ser_vec(&s, ProtocolVersion::local())?;
		let saved = self
			.write(SWAP_PREFIX, &s.input.commit, &data, overwrite)
			.map_err(StoreError::WriteError)?;
		if !saved {
			Err(StoreError::AlreadyExists(s.input.commit.clone()))
		} else {
			Ok(())
		}
	}

	/// Iterator over all swaps.
	pub fn swaps_iter(&self) -> Result<impl Iterator<Item = SwapData>, StoreError> {
		let key = store::to_key(SWAP_PREFIX, "");
		let protocol_version = self.db.protocol_version();
		self.db
			.iter(&key[..], move |_, mut v| {
				ser::deserialize(&mut v, protocol_version, DeserializationMode::default())
					.map_err(From::from)
			})
			.map_err(|e| StoreError::ReadError(e))
	}

	/// Checks if a matching swap exists in the database
	#[allow(dead_code)]
	pub fn swap_exists(&self, input_commit: &Commitment) -> Result<bool, StoreError> {
		let key = store::to_key(SWAP_PREFIX, input_commit);
		self.db
			.batch()
			.map_err(StoreError::ReadError)?
			.exists(&key[..])
			.map_err(StoreError::ReadError)
	}

	/// Reads a swap from the database
	#[allow(dead_code)]
	pub fn get_swap(&self, input_commit: &Commitment) -> Result<SwapData, StoreError> {
		self.read(SWAP_PREFIX, input_commit)
	}
}

#[cfg(test)]
mod tests {
	use crate::onion::test_util::rand_onion;
	use crate::secp::test_util::{rand_commit, rand_hash, rand_proof};
	use crate::store::{SwapData, SwapStatus, SwapStore};
	use crate::{secp, StoreError};
	use grin_core::core::{Input, OutputFeatures};
	use grin_core::global::{self, ChainTypes};
	use rand::RngCore;
	use std::cmp::Ordering;

	fn new_store(test_name: &str) -> SwapStore {
		global::set_local_chain_type(ChainTypes::AutomatedTesting);
		let db_root = format!("./target/tmp/.{}", test_name);
		let _ = std::fs::remove_dir_all(db_root.as_str());
		SwapStore::new(db_root.as_str()).unwrap()
	}

	fn rand_swap_with_status(status: SwapStatus) -> SwapData {
		SwapData {
			excess: secp::random_secret(),
			output_commit: rand_commit(),
			rangeproof: Some(rand_proof()),
			input: Input::new(OutputFeatures::Plain, rand_commit()),
			fee: rand::thread_rng().next_u64(),
			onion: rand_onion(),
			status,
		}
	}

	fn rand_swap() -> SwapData {
		let s = rand::thread_rng().next_u64() % 3;
		let status = if s == 0 {
			SwapStatus::Unprocessed
		} else if s == 1 {
			SwapStatus::InProcess {
				kernel_hash: rand_hash(),
			}
		} else {
			SwapStatus::Completed {
				kernel_hash: rand_hash(),
				block_hash: rand_hash(),
			}
		};
		rand_swap_with_status(status)
	}

	#[test]
	fn swap_iter() -> Result<(), Box<dyn std::error::Error>> {
		let store = new_store("swap_iter");
		let mut swaps: Vec<SwapData> = Vec::new();
		for _ in 0..5 {
			let swap = rand_swap();
			store.save_swap(&swap, false)?;
			swaps.push(swap);
		}

		swaps.sort_by(|a, b| {
			if a.input.commit < b.input.commit {
				Ordering::Less
			} else if a.input.commit == b.input.commit {
				Ordering::Equal
			} else {
				Ordering::Greater
			}
		});

		let mut i: usize = 0;
		for swap in store.swaps_iter()? {
			assert_eq!(swap, *swaps.get(i).unwrap());
			i += 1;
		}

		Ok(())
	}

	#[test]
	fn save_swap() -> Result<(), Box<dyn std::error::Error>> {
		let store = new_store("save_swap");

		let mut swap = rand_swap_with_status(SwapStatus::Unprocessed);
		assert!(!store.swap_exists(&swap.input.commit)?);

		store.save_swap(&swap, false)?;
		assert_eq!(swap, store.get_swap(&swap.input.commit)?);
		assert!(store.swap_exists(&swap.input.commit)?);

		swap.status = SwapStatus::InProcess {
			kernel_hash: rand_hash(),
		};
		let result = store.save_swap(&swap, false);
		assert_eq!(
			Err(StoreError::AlreadyExists(swap.input.commit.clone())),
			result
		);

		store.save_swap(&swap, true)?;
		assert_eq!(swap, store.get_swap(&swap.input.commit)?);

		Ok(())
	}
}
