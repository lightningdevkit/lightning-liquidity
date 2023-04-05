use std::fmt::Write;

#[inline]
pub fn hex_str(value: &[u8]) -> String {
	let mut res = String::with_capacity(2 * value.len());
	for v in value {
		write!(&mut res, "{:02x}", v).expect("Unable to write");
	}
	res
}
