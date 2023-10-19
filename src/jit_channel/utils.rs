use bitcoin::hashes::hmac::{Hmac, HmacEngine};
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::{Hash, HashEngine};

use crate::jit_channel::msgs::OpeningFeeParams;
use crate::utils;

/// Determines if the given parameters are valid given the secret used to generate the promise.
// TODO: add validation check that valid_until >= now()
pub fn is_valid_opening_fee_params(
	fee_params: &OpeningFeeParams, promise_secret: &[u8; 32],
) -> bool {
	let mut hmac = HmacEngine::<Sha256>::new(promise_secret);
	hmac.input(&fee_params.min_fee_msat.to_be_bytes());
	hmac.input(&fee_params.proportional.to_be_bytes());
	hmac.input(fee_params.valid_until.to_rfc3339().as_bytes());
	hmac.input(&fee_params.min_lifetime.to_be_bytes());
	hmac.input(&fee_params.max_client_to_self_delay.to_be_bytes());
	let promise_bytes = Hmac::from_engine(hmac).into_inner();
	let promise = utils::hex_str(&promise_bytes[..]);
	promise == fee_params.promise
}

/// Computes the opening fee given a payment size and the fee parameters.
///
/// Returns [`Option::None`] when the computation overflows.
///
/// See the [`specification`](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS2#computing-the-opening_fee) for more details.
pub fn compute_opening_fee(
	payment_size_msat: u64, opening_fee_min_fee_msat: u64, opening_fee_proportional: u32,
) -> Option<u64> {
	let t1 = payment_size_msat.checked_mul(opening_fee_proportional.into())?;
	let t2 = t1.checked_add(999999)?;
	let t3 = t2.checked_div(1000000)?;
	let t4 = std::cmp::max(t3, opening_fee_min_fee_msat);
	Some(t4)
}
