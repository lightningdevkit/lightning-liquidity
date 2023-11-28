use bitcoin::secp256k1::PublicKey;
use core::{fmt::Write, ops::Deref};
use lightning::io;
use lightning::sign::EntropySource;

use crate::lsps0::msgs::RequestId;
use crate::prelude::{String, Vec};

/// Maximum transaction index that can be used in a `short_channel_id`.
/// This value is based on the 3-bytes available for tx index.
pub const MAX_SCID_TX_INDEX: u64 = 0x00ffffff;

/// Maximum vout index that can be used in a `short_channel_id`. This
/// value is based on the 2-bytes available for the vout index.
pub const MAX_SCID_VOUT_INDEX: u64 = 0xffff;

/// Extracts the block height (most significant 3-bytes) from the `short_channel_id`.
pub fn block_from_scid(short_channel_id: &u64) -> u32 {
	(short_channel_id >> 40) as u32
}

/// Extracts the tx index (bytes [2..4]) from the `short_channel_id`.
pub fn tx_index_from_scid(short_channel_id: &u64) -> u32 {
	((short_channel_id >> 16) & MAX_SCID_TX_INDEX) as u32
}

/// Extracts the vout (bytes [0..2]) from the `short_channel_id`.
pub fn vout_from_scid(short_channel_id: &u64) -> u16 {
	((short_channel_id) & MAX_SCID_VOUT_INDEX) as u16
}

pub fn scid_from_human_readable_string(human_readable_scid: &str) -> Result<u64, ()> {
	let mut parts = human_readable_scid.split('x');

	let block: u64 = parts.next().ok_or(())?.parse().map_err(|_e| ())?;
	let tx_index: u64 = parts.next().ok_or(())?.parse().map_err(|_e| ())?;
	let vout_index: u64 = parts.next().ok_or(())?.parse().map_err(|_e| ())?;

	Ok((block << 40) | (tx_index << 16) | vout_index)
}

pub(crate) fn generate_request_id<ES: Deref>(entropy_source: &ES) -> RequestId
where
	ES::Target: EntropySource,
{
	let bytes = entropy_source.get_secure_random_bytes();
	RequestId(hex_str(&bytes[0..16]))
}

#[inline]
pub fn hex_str(value: &[u8]) -> String {
	let mut res = String::with_capacity(2 * value.len());
	for v in value {
		write!(&mut res, "{:02x}", v).expect("Unable to write");
	}
	res
}

pub fn to_vec(hex: &str) -> Option<Vec<u8>> {
	let mut out = Vec::with_capacity(hex.len() / 2);

	let mut b = 0;
	for (idx, c) in hex.as_bytes().iter().enumerate() {
		b <<= 4;
		match *c {
			b'A'..=b'F' => b |= c - b'A' + 10,
			b'a'..=b'f' => b |= c - b'a' + 10,
			b'0'..=b'9' => b |= c - b'0',
			_ => return None,
		}
		if (idx & 1) == 1 {
			out.push(b);
			b = 0;
		}
	}

	Some(out)
}

pub fn to_compressed_pubkey(hex: &str) -> Option<PublicKey> {
	if hex.len() != 33 * 2 {
		return None;
	}
	let data = match to_vec(&hex[0..33 * 2]) {
		Some(bytes) => bytes,
		None => return None,
	};
	match PublicKey::from_slice(&data) {
		Ok(pk) => Some(pk),
		Err(_) => None,
	}
}

pub fn parse_pubkey(pubkey_str: &str) -> Result<PublicKey, io::Error> {
	let pubkey = to_compressed_pubkey(pubkey_str);
	if pubkey.is_none() {
		return Err(io::Error::new(
			io::ErrorKind::Other,
			"ERROR: unable to parse given pubkey for node",
		));
	}

	Ok(pubkey.unwrap())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_human_readable_scid_correctly() {
		let block = 140;
		let tx_index = 123;
		let vout = 22;

		let human_readable_scid = format!("{}x{}x{}", block, tx_index, vout);

		let scid = scid_from_human_readable_string(&human_readable_scid).unwrap();

		assert_eq!(block_from_scid(&scid), block);
		assert_eq!(tx_index_from_scid(&scid), tx_index);
		assert_eq!(vout_from_scid(&scid), vout);
	}
}
