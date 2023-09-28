use std::convert::TryFrom;

use bitcoin::hashes::hmac::{Hmac, HmacEngine};
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::{Hash, HashEngine};
use serde::{Deserialize, Serialize};

use crate::transport::msgs::{LSPSMessage, RequestId, ResponseError};
use crate::utils;

pub(crate) const LSPS2_GET_VERSIONS_METHOD_NAME: &str = "lsps2.get_versions";
pub(crate) const LSPS2_GET_INFO_METHOD_NAME: &str = "lsps2.get_info";
pub(crate) const LSPS2_BUY_METHOD_NAME: &str = "lsps2.buy";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct GetVersionsRequest {}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetVersionsResponse {
	pub versions: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetInfoRequest {
	pub version: u16,
	pub token: Option<String>,
}

/// Fees and parameters for a JIT Channel.
///
/// The client will pay max(min_fee_msat, proportional*(payment_size_msat/1_000_000)).
pub struct RawOpeningFeeParams {
	/// The minimum fee required for the channel open.
	pub min_fee_msat: u64,
	/// A fee proportional to the size of the initial payment.
	pub proportional: u32,
	/// An ISO8601 formatted date for which these params are valid.
	pub valid_until: String,
	/// number of blocks that the LSP promises it will keep the channel alive without closing, after confirmation.
	pub min_lifetime: u32,
	/// Maximum number of blocks that the client is allowed to set its to_self_delay parameter.
	pub max_client_to_self_delay: u32,
}

impl RawOpeningFeeParams {
	pub(crate) fn into_opening_fee_params(self, promise_secret: &[u8; 32]) -> OpeningFeeParams {
		let mut hmac = HmacEngine::<Sha256>::new(promise_secret);
		hmac.input(&self.min_fee_msat.to_be_bytes());
		hmac.input(&self.proportional.to_be_bytes());
		hmac.input(self.valid_until.as_bytes());
		hmac.input(&self.min_lifetime.to_be_bytes());
		hmac.input(&self.max_client_to_self_delay.to_be_bytes());
		let promise_bytes = Hmac::from_engine(hmac).into_inner();
		let promise = utils::hex_str(&promise_bytes[..]);
		OpeningFeeParams {
			min_fee_msat: self.min_fee_msat,
			proportional: self.proportional,
			valid_until: self.valid_until.clone(),
			min_lifetime: self.min_lifetime,
			max_client_to_self_delay: self.max_client_to_self_delay,
			promise,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
/// Fees and parameters for a JIT Channel with promise.
///
/// The client will pay max(min_fee_msat, proportional*(payment_size_msat/1_000_000)).
pub struct OpeningFeeParams {
	/// The minimum fee required for the channel open.
	pub min_fee_msat: u64,
	/// A fee proportional to the size of the initial payment.
	pub proportional: u32,
	/// An ISO8601 formatted date for which these params are valid.
	pub valid_until: String,
	/// number of blocks that the LSP promises it will keep the channel alive without closing, after confirmation.
	pub min_lifetime: u32,
	/// Maximum number of blocks that the client is allowed to set its to_self_delay parameter.
	pub max_client_to_self_delay: u32,
	/// Field used by the LSP to validate that these parameters were actually given out by them.
	pub promise: String,
}

impl OpeningFeeParams {
	/// Determine that these parameters are valid given the secret used to generate the promise.
	pub fn is_valid(&self, promise_secret: &[u8; 32]) -> bool {
		let mut hmac = HmacEngine::<Sha256>::new(promise_secret);
		hmac.input(&self.min_fee_msat.to_be_bytes());
		hmac.input(&self.proportional.to_be_bytes());
		hmac.input(self.valid_until.as_bytes());
		hmac.input(&self.min_lifetime.to_be_bytes());
		hmac.input(&self.max_client_to_self_delay.to_be_bytes());
		let promise_bytes = Hmac::from_engine(hmac).into_inner();
		let promise = utils::hex_str(&promise_bytes[..]);
		promise == self.promise
	}
}

/// Information about the parameters a LSP is willing to offer clients
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetInfoResponse {
	/// A set of opening fee parameters.
	pub opening_fee_params_menu: Vec<OpeningFeeParams>,
	/// The minimum payment size required to open a channel.
	pub min_payment_size_msat: u64,
	/// The maximum payment size the lsp will tolerate.
	pub max_payment_size_msat: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuyRequest {
	pub version: u16,
	pub opening_fee_params: OpeningFeeParams,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub payment_size_msat: Option<u64>,
}

/// A response from a buy request made by a client
///
/// Includes information needed to construct an invoice.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuyResponse {
	/// The short channel id used by LSP to identify need to open channel.
	pub jit_channel_scid: String,
	/// The locktime expiry delta the lsp requires.
	pub lsp_cltv_expiry_delta: u32,
	/// A flag that indicates who is trusting who.
	#[serde(default)]
	pub client_trusts_lsp: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
	GetVersions(GetVersionsRequest),
	GetInfo(GetInfoRequest),
	Buy(BuyRequest),
}

impl Request {
	pub fn method(&self) -> &str {
		match self {
			Request::GetVersions(_) => LSPS2_GET_VERSIONS_METHOD_NAME,
			Request::GetInfo(_) => LSPS2_GET_INFO_METHOD_NAME,
			Request::Buy(_) => LSPS2_BUY_METHOD_NAME,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
	GetVersions(GetVersionsResponse),
	GetInfo(GetInfoResponse),
	GetInfoError(ResponseError),
	Buy(BuyResponse),
	BuyError(ResponseError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
	Request(RequestId, Request),
	Response(RequestId, Response),
}

impl TryFrom<LSPSMessage> for Message {
	type Error = ();

	fn try_from(message: LSPSMessage) -> Result<Self, Self::Error> {
		if let LSPSMessage::LSPS2(message) = message {
			return Ok(message);
		}

		Err(())
	}
}

impl From<Message> for LSPSMessage {
	fn from(message: Message) -> Self {
		LSPSMessage::LSPS2(message)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn into_opening_fee_params_produces_valid_promise() {
		let min_fee_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let min_lifetime = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			min_fee_msat,
			proportional,
			valid_until: valid_until.clone(),
			min_lifetime,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];

		let opening_fee_params = raw.into_opening_fee_params(&promise_secret);

		assert_eq!(opening_fee_params.min_fee_msat, min_fee_msat);
		assert_eq!(opening_fee_params.proportional, proportional);
		assert_eq!(opening_fee_params.valid_until, valid_until);
		assert_eq!(opening_fee_params.min_lifetime, min_lifetime);
		assert_eq!(opening_fee_params.max_client_to_self_delay, max_client_to_self_delay);

		assert!(opening_fee_params.is_valid(&promise_secret));
	}

	#[test]
	fn changing_single_field_produced_invalid_params() {
		let min_fee_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let min_lifetime = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			min_fee_msat,
			proportional,
			valid_until,
			min_lifetime,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];

		let mut opening_fee_params = raw.into_opening_fee_params(&promise_secret);
		opening_fee_params.min_fee_msat = min_fee_msat + 1;
		assert!(!opening_fee_params.is_valid(&promise_secret));
	}

	#[test]
	fn wrong_secret_produced_invalid_params() {
		let min_fee_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let min_lifetime = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			min_fee_msat,
			proportional,
			valid_until,
			min_lifetime,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];
		let other_secret = [2u8; 32];

		let opening_fee_params = raw.into_opening_fee_params(&promise_secret);
		assert!(!opening_fee_params.is_valid(&other_secret));
	}
}
