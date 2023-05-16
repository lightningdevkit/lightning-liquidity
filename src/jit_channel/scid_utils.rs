#[derive(Debug, PartialEq, Eq)]
pub enum ShortChannelIdError {
	InvalidScid,
}

/// Maximum transaction index that can be used in a `short_channel_id`.
/// This value is based on the 3-bytes available for tx index.
pub const MAX_SCID_TX_INDEX: u64 = 0x00ffffff;

/// Maximum vout index that can be used in a `short_channel_id`. This
/// value is based on the 2-bytes available for the vout index.
pub const MAX_SCID_VOUT_INDEX: u64 = 0xffff;

/// Extracts the block height (most significant 3-bytes) from the `short_channel_id`
pub fn block_from_scid(short_channel_id: &u64) -> u32 {
	(short_channel_id >> 40) as u32
}

/// Extracts the tx index (bytes [2..4]) from the `short_channel_id`
pub fn tx_index_from_scid(short_channel_id: &u64) -> u32 {
	((short_channel_id >> 16) & MAX_SCID_TX_INDEX) as u32
}

/// Extracts the vout (bytes [0..2]) from the `short_channel_id`
pub fn vout_from_scid(short_channel_id: &u64) -> u16 {
	((short_channel_id) & MAX_SCID_VOUT_INDEX) as u16
}

pub fn scid_from_human_readable_string(
	human_readable_scid: &str,
) -> Result<u64, ShortChannelIdError> {
	let mut parts = human_readable_scid.split('x');

	let block: u64 = parts
		.next()
		.ok_or(ShortChannelIdError::InvalidScid)?
		.parse()
		.map_err(|_e| ShortChannelIdError::InvalidScid)?;
	let tx_index: u64 = parts
		.next()
		.ok_or(ShortChannelIdError::InvalidScid)?
		.parse()
		.map_err(|_e| ShortChannelIdError::InvalidScid)?;
	let vout_index: u64 = parts
		.next()
		.ok_or(ShortChannelIdError::InvalidScid)?
		.parse()
		.map_err(|_e| ShortChannelIdError::InvalidScid)?;

	Ok((block << 40) | (tx_index << 16) | vout_index)
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
