pub use secp256k1zkp::aggsig;
pub use secp256k1zkp::constants::{
	AGG_SIGNATURE_SIZE, COMPRESSED_PUBLIC_KEY_SIZE, MAX_PROOF_SIZE, PEDERSEN_COMMITMENT_SIZE,
	SECRET_KEY_SIZE,
};
pub use secp256k1zkp::ecdh::SharedSecret;
pub use secp256k1zkp::key::{PublicKey, SecretKey, ZERO_KEY};
pub use secp256k1zkp::pedersen::{Commitment, RangeProof};
pub use secp256k1zkp::{ContextFlag, Message, Secp256k1, Signature};

use blake2::blake2b::Blake2b;
use byteorder::{BigEndian, ByteOrder};
use grin_core::ser::{self, Readable, Reader, Writeable, Writer};
use secp256k1zkp::rand::thread_rng;
use thiserror::Error;

/// A generalized Schnorr signature with a pedersen commitment value & blinding factors as the keys
#[derive(Clone)]
pub struct ComSignature {
	pub_nonce: Commitment,
	s: SecretKey,
	t: SecretKey,
}

/// Error types for Commitment Signatures
#[derive(Error, Debug)]
pub enum ComSigError {
	#[error("Commitment signature is invalid")]
	InvalidSig,
	#[error("Secp256k1zkp error: {0:?}")]
	Secp256k1zkp(secp256k1zkp::Error),
}

impl From<secp256k1zkp::Error> for ComSigError {
	fn from(err: secp256k1zkp::Error) -> ComSigError {
		ComSigError::Secp256k1zkp(err)
	}
}

impl ComSignature {
	pub fn new(pub_nonce: &Commitment, s: &SecretKey, t: &SecretKey) -> ComSignature {
		ComSignature {
			pub_nonce: pub_nonce.to_owned(),
			s: s.to_owned(),
			t: t.to_owned(),
		}
	}

	#[allow(dead_code)]
	pub fn sign(
		amount: u64,
		blind: &SecretKey,
		msg: &Vec<u8>,
	) -> Result<ComSignature, ComSigError> {
		let secp = Secp256k1::with_caps(ContextFlag::Commit);

		let mut amt_bytes = [0; 32];
		BigEndian::write_u64(&mut amt_bytes[24..32], amount);
		let k_amt = SecretKey::from_slice(&secp, &amt_bytes)?;

		let k_1 = SecretKey::new(&secp, &mut thread_rng());
		let k_2 = SecretKey::new(&secp, &mut thread_rng());

		let commitment = secp.commit(amount, blind.clone())?;
		let nonce_commitment = secp.commit_blind(k_1.clone(), k_2.clone())?;

		let e = ComSignature::calc_challenge(&secp, &commitment, &nonce_commitment, &msg)?;

		// s = k_1 + (e * amount)
		let mut s = k_amt.clone();
		s.mul_assign(&secp, &e)?;
		s.add_assign(&secp, &k_1)?;

		// t = k_2 + (e * blind)
		let mut t = blind.clone();
		t.mul_assign(&secp, &e)?;
		t.add_assign(&secp, &k_2)?;

		Ok(ComSignature::new(&nonce_commitment, &s, &t))
	}

	#[allow(non_snake_case)]
	pub fn verify(&self, commit: &Commitment, msg: &Vec<u8>) -> Result<(), ComSigError> {
		let secp = Secp256k1::with_caps(ContextFlag::Commit);

		let S1 = secp.commit_blind(self.s.clone(), self.t.clone())?;

		let mut Ce = commit.to_pubkey(&secp)?;
		let e = ComSignature::calc_challenge(&secp, &commit, &self.pub_nonce, &msg)?;
		Ce.mul_assign(&secp, &e)?;

		let commits = vec![Commitment::from_pubkey(&secp, &Ce)?, self.pub_nonce.clone()];
		let S2 = secp.commit_sum(commits, Vec::new())?;

		if S1 != S2 {
			return Err(ComSigError::InvalidSig);
		}

		Ok(())
	}

	fn calc_challenge(
		secp: &Secp256k1,
		commit: &Commitment,
		nonce_commit: &Commitment,
		msg: &Vec<u8>,
	) -> Result<SecretKey, ComSigError> {
		let mut challenge_hasher = Blake2b::new(32);
		challenge_hasher.update(&commit.0);
		challenge_hasher.update(&nonce_commit.0);
		challenge_hasher.update(msg);

		let mut challenge = [0; 32];
		challenge.copy_from_slice(challenge_hasher.finalize().as_bytes());

		Ok(SecretKey::from_slice(&secp, &challenge)?)
	}
}

/// Serializes a ComSignature to and from hex
pub mod comsig_serde {
	use super::ComSignature;
	use grin_core::ser::{self, ProtocolVersion};
	use grin_util::ToHex;
	use serde::{Deserialize, Serializer};

	/// Serializes a ComSignature as a hex string
	pub fn serialize<S>(comsig: &ComSignature, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		use serde::ser::Error;
		let bytes = ser::ser_vec(&comsig, ProtocolVersion::local()).map_err(Error::custom)?;
		serializer.serialize_str(&bytes.to_hex())
	}

	/// Creates a ComSignature from a hex string
	pub fn deserialize<'de, D>(deserializer: D) -> Result<ComSignature, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		use serde::de::Error;
		let bytes = String::deserialize(deserializer)
			.and_then(|string| grin_util::from_hex(&string).map_err(Error::custom))?;
		let sig: ComSignature = ser::deserialize_default(&mut &bytes[..]).map_err(Error::custom)?;
		Ok(sig)
	}
}

#[allow(non_snake_case)]
impl Readable for ComSignature {
	fn read<R: Reader>(reader: &mut R) -> Result<Self, ser::Error> {
		let R = Commitment::read(reader)?;
		let s = read_secret_key(reader)?;
		let t = read_secret_key(reader)?;
		Ok(ComSignature::new(&R, &s, &t))
	}
}

impl Writeable for ComSignature {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		writer.write_fixed_bytes(self.pub_nonce.0)?;
		writer.write_fixed_bytes(self.s.0)?;
		writer.write_fixed_bytes(self.t.0)?;
		Ok(())
	}
}

/// Generate a random SecretKey.
pub fn random_secret() -> SecretKey {
	let secp = Secp256k1::new();
	SecretKey::new(&secp, &mut thread_rng())
}

/// Deserialize a SecretKey from a Reader
pub fn read_secret_key<R: Reader>(reader: &mut R) -> Result<SecretKey, ser::Error> {
	let buf = reader.read_fixed_bytes(SECRET_KEY_SIZE)?;
	let secp = Secp256k1::with_caps(ContextFlag::None);
	let pk = SecretKey::from_slice(&secp, &buf).map_err(|_| ser::Error::CorruptedData)?;
	Ok(pk)
}

/// Build a Pedersen Commitment using the provided value and blinding factor
pub fn commit(value: u64, blind: &SecretKey) -> Result<Commitment, secp256k1zkp::Error> {
	let secp = Secp256k1::with_caps(ContextFlag::Commit);
	let commit = secp.commit(value, blind.clone())?;
	Ok(commit)
}

/// Add a blinding factor to an existing Commitment
pub fn add_excess(
	commitment: &Commitment,
	excess: &SecretKey,
) -> Result<Commitment, secp256k1zkp::Error> {
	let secp = Secp256k1::with_caps(ContextFlag::Commit);
	let excess_commit: Commitment = secp.commit(0, excess.clone())?;

	let commits = vec![commitment.clone(), excess_commit.clone()];
	let sum = secp.commit_sum(commits, Vec::new())?;
	Ok(sum)
}

/// Subtracts a value (v*H) from an existing commitment
pub fn sub_value(commitment: &Commitment, value: u64) -> Result<Commitment, secp256k1zkp::Error> {
	let secp = Secp256k1::with_caps(ContextFlag::Commit);
	let neg_commit: Commitment = secp.commit(value, ZERO_KEY)?;
	let sum = secp.commit_sum(vec![commitment.clone()], vec![neg_commit.clone()])?;
	Ok(sum)
}

/// Signs the message with the provided SecretKey
pub fn sign(sk: &SecretKey, msg: &Message) -> Result<Signature, secp256k1zkp::Error> {
	let secp = Secp256k1::with_caps(ContextFlag::Full);
	let pubkey = PublicKey::from_secret_key(&secp, &sk)?;
	let sig = aggsig::sign_single(&secp, &msg, &sk, None, None, None, Some(&pubkey), None)?;
	Ok(sig)
}

#[cfg(test)]
pub mod test_util {
	use crate::secp::{self, Commitment, PublicKey, RangeProof, Secp256k1};
	use grin_core::core::hash::Hash;
	use grin_util::ToHex;
	use rand::RngCore;

	pub fn rand_commit() -> Commitment {
		secp::commit(rand::thread_rng().next_u64(), &secp::random_secret()).unwrap()
	}

	pub fn rand_hash() -> Hash {
		Hash::from_hex(secp::random_secret().to_hex().as_str()).unwrap()
	}

	pub fn rand_proof() -> RangeProof {
		let secp = Secp256k1::new();
		secp.bullet_proof(
			rand::thread_rng().next_u64(),
			secp::random_secret(),
			secp::random_secret(),
			secp::random_secret(),
			None,
			None,
		)
	}

	pub fn rand_pubkey() -> PublicKey {
		let secp = Secp256k1::new();
		PublicKey::from_secret_key(&secp, &secp::random_secret()).unwrap()
	}
}

#[cfg(test)]
mod tests {
	use super::{ComSigError, ComSignature, ContextFlag, Secp256k1, SecretKey};

	use rand::Rng;
	use secp256k1zkp::rand::{thread_rng, RngCore};

	/// Test signing and verification of ComSignatures
	#[test]
	fn verify_comsig() -> Result<(), ComSigError> {
		let secp = Secp256k1::with_caps(ContextFlag::Commit);

		let amount = thread_rng().next_u64();
		let blind = SecretKey::new(&secp, &mut thread_rng());
		let msg: [u8; 16] = rand::thread_rng().gen();
		let comsig = ComSignature::sign(amount, &blind, &msg.to_vec())?;

		let commit = secp.commit(amount, blind.clone())?;
		assert!(comsig.verify(&commit, &msg.to_vec()).is_ok());

		let wrong_msg: [u8; 16] = rand::thread_rng().gen();
		assert!(comsig.verify(&commit, &wrong_msg.to_vec()).is_err());

		let wrong_commit = secp.commit(amount, SecretKey::new(&secp, &mut thread_rng()))?;
		assert!(comsig.verify(&wrong_commit, &msg.to_vec()).is_err());

		Ok(())
	}
}
