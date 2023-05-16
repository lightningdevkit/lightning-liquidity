use std::convert::TryFrom;

use bitcoin::hashes::hmac::{Hmac, HmacEngine};
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::{Hash, HashEngine};
use serde::{Deserialize, Serialize};

use crate::transport::msgs::{LSPSMessage, RequestId, ResponseError};
use crate::utils;

pub(crate) const LSPS2_GETVERSIONS_METHOD_NAME: &str = "lsps2.getversions";
pub(crate) const LSPS2_GETINFO_METHOD_NAME: &str = "lsps2.getinfo";
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

pub struct RawOpeningFeeParams {
	pub base_msat: u64,
	pub proportional: u32,
	pub valid_until: String,
	pub max_idle_time: u32,
	pub max_client_to_self_delay: u32,
}

impl RawOpeningFeeParams {
	pub fn into_opening_fee_params(self, promise_secret: &[u8; 32]) -> OpeningFeeParams {
		let mut hmac = HmacEngine::<Sha256>::new(promise_secret);
		hmac.input(&self.base_msat.to_be_bytes());
		hmac.input(&self.proportional.to_be_bytes());
		hmac.input(self.valid_until.as_bytes());
		hmac.input(&self.max_idle_time.to_be_bytes());
		hmac.input(&self.max_client_to_self_delay.to_be_bytes());
		let promise_bytes = Hmac::from_engine(hmac).into_inner();
		let promise = utils::hex_str(&promise_bytes[..]);
		OpeningFeeParams {
			base_msat: self.base_msat,
			proportional: self.proportional,
			valid_until: self.valid_until.clone(),
			max_idle_time: self.max_idle_time,
			max_client_to_self_delay: self.max_client_to_self_delay,
			promise,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OpeningFeeParams {
	pub base_msat: u64,
	pub proportional: u32,
	pub valid_until: String,
	pub max_idle_time: u32,
	pub max_client_to_self_delay: u32,
	pub promise: String,
}

impl OpeningFeeParams {
	pub fn is_valid(&self, promise_secret: &[u8; 32]) -> bool {
		let mut hmac = HmacEngine::<Sha256>::new(promise_secret);
		hmac.input(&self.base_msat.to_be_bytes());
		hmac.input(&self.proportional.to_be_bytes());
		hmac.input(self.valid_until.as_bytes());
		hmac.input(&self.max_idle_time.to_be_bytes());
		hmac.input(&self.max_client_to_self_delay.to_be_bytes());
		let promise_bytes = Hmac::from_engine(hmac).into_inner();
		let promise = utils::hex_str(&promise_bytes[..]);
		promise == self.promise
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetInfoResponse {
	pub opening_fee_params_menu: Vec<OpeningFeeParams>,
	pub min_payment_size_msat: u64,
	pub max_payment_size_msat: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuyRequest {
	pub version: u16,
	pub opening_fee_params: OpeningFeeParams,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub payment_size_msat: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuyResponse {
	pub jit_channel_scid: String,
	pub lsp_cltv_expiry_delta: u32,
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
			Request::GetVersions(_) => LSPS2_GETVERSIONS_METHOD_NAME,
			Request::GetInfo(_) => LSPS2_GETINFO_METHOD_NAME,
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
		let base_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let max_idle_time = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			base_msat,
			proportional,
			valid_until: valid_until.clone(),
			max_idle_time,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];

		let opening_fee_params = raw.into_opening_fee_params(&promise_secret);

		assert_eq!(opening_fee_params.base_msat, base_msat);
		assert_eq!(opening_fee_params.proportional, proportional);
		assert_eq!(opening_fee_params.valid_until, valid_until);
		assert_eq!(opening_fee_params.max_idle_time, max_idle_time);
		assert_eq!(opening_fee_params.max_client_to_self_delay, max_client_to_self_delay);

		assert!(opening_fee_params.is_valid(&promise_secret));
	}

	#[test]
	fn changing_single_field_produced_invalid_params() {
		let base_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let max_idle_time = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			base_msat,
			proportional,
			valid_until,
			max_idle_time,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];

		let mut opening_fee_params = raw.into_opening_fee_params(&promise_secret);
		opening_fee_params.base_msat = base_msat + 1;
		assert!(!opening_fee_params.is_valid(&promise_secret));
	}

	#[test]
	fn wrong_secret_produced_invalid_params() {
		let base_msat = 100;
		let proportional = 21;
		let valid_until = "2023-05-20".to_string();
		let max_idle_time = 144;
		let max_client_to_self_delay = 128;

		let raw = RawOpeningFeeParams {
			base_msat,
			proportional,
			valid_until,
			max_idle_time,
			max_client_to_self_delay,
		};

		let promise_secret = [1u8; 32];
		let other_secret = [2u8; 32];

		let opening_fee_params = raw.into_opening_fee_params(&promise_secret);
		assert!(!opening_fee_params.is_valid(&other_secret));
	}
}
