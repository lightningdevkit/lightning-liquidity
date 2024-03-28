//! Message, request, and other primitive types used to implement LSPS1.

use crate::lsps0::ser::{
	string_amount, string_amount_option, LSPSMessage, RequestId, ResponseError,
};

use crate::prelude::{String, Vec};

use bitcoin::address::{Address, NetworkUnchecked};

use lightning_invoice::Bolt11Invoice;

use serde::{Deserialize, Serialize};

use chrono::Utc;

use core::convert::TryFrom;

pub(crate) const LSPS1_GET_INFO_METHOD_NAME: &str = "lsps1.get_info";
pub(crate) const LSPS1_CREATE_ORDER_METHOD_NAME: &str = "lsps1.create_order";
pub(crate) const LSPS1_GET_ORDER_METHOD_NAME: &str = "lsps1.get_order";

pub(crate) const LSPS1_CREATE_ORDER_REQUEST_INVALID_PARAMS_ERROR_CODE: i32 = -32602;
pub(crate) const LSPS1_CREATE_ORDER_REQUEST_ORDER_MISMATCH_ERROR_CODE: i32 = 100;

/// The identifier of an order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Hash)]
pub struct OrderId(pub String);

/// A request made to an LSP to retrieve the supported options.
///
/// Please refer to the [LSPS1 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS1#1-lsps1info)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct GetInfoRequest {}

/// An object representing the supported protocol options.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OptionsSupported {
	/// The minimum number of block confirmations before the LSP accepts a channel as confirmed.
	pub min_channel_confirmations: u8,
	/// The minimum number of block confirmations before the LSP accepts an on-chain payment as confirmed.
	pub min_onchain_payment_confirmations: Option<u8>,
	/// Indicates if the LSP supports zero reserve.
	pub supports_zero_channel_reserve: bool,
	/// Indicates the minimum amount of satoshi that is required for the LSP to accept a payment
	/// on-chain.
	#[serde(with = "string_amount_option")]
	pub min_onchain_payment_size_sat: Option<u64>,
	/// The maximum number of blocks a channel can be leased for.
	pub max_channel_expiry_blocks: u32,
	/// The minimum number of satoshi that the client MUST request.
	#[serde(with = "string_amount")]
	pub min_initial_client_balance_sat: u64,
	/// The maximum number of satoshi that the client MUST request.
	#[serde(with = "string_amount")]
	pub max_initial_client_balance_sat: u64,
	/// The minimum number of satoshi that the LSP will provide to the channel.
	#[serde(with = "string_amount")]
	pub min_initial_lsp_balance_sat: u64,
	/// The maximum number of satoshi that the LSP will provide to the channel.
	#[serde(with = "string_amount")]
	pub max_initial_lsp_balance_sat: u64,
	/// The minimal channel size.
	#[serde(with = "string_amount")]
	pub min_channel_balance_sat: u64,
	/// The maximal channel size.
	#[serde(with = "string_amount")]
	pub max_channel_balance_sat: u64,
}

/// A response to an [`GetInfoRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetInfoResponse {
	/// The website of the LSP.
	pub website: String,
	/// All options supported by the LSP.
	pub options: OptionsSupported,
}

/// A request made to an LSP to create an order.
///
/// Please refer to the [LSPS1 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS1#2-lsps1create_order)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CreateOrderRequest {
	/// The order made.
	pub order: OrderParams,
}

/// An object representing an LSPS1 channel order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OrderParams {
	/// Indicates how many satoshi the LSP will provide on their side.
	#[serde(with = "string_amount")]
	pub lsp_balance_sat: u64,
	/// Indicates how many satoshi the client will provide on their side.
	///
	/// The client sends these funds to the LSP, who will push them back to the client upon opening
	/// the channel.
	#[serde(with = "string_amount")]
	pub client_balance_sat: u64,
	/// The number of blocks the client wants to wait maximally for the channel to be confirmed.
	pub confirms_within_blocks: u32,
	/// Indicates how long the channel is leased for in block time.
	pub channel_expiry_blocks: u32,
	/// May contain arbitrary associated data like a coupon code or a authentication token.
	pub token: String,
	/// The address where the LSP will send the funds if the order fails.
	pub refund_onchain_address: Option<Address<NetworkUnchecked>>,
	/// Indicates if the channel should be announced to the network.
	pub announce_channel: bool,
}

/// A response to a [`CreateOrderRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CreateOrderResponse {
	/// The id of the channel order.
	pub order_id: OrderId,
	/// The parameters of channel order.
	pub order: OrderParams,
	/// The datetime when the order was created
	pub created_at: chrono::DateTime<Utc>,
	/// The datetime when the order expires.
	pub expires_at: chrono::DateTime<Utc>,
	/// The current state of the order.
	pub order_state: OrderState,
	/// Contains details about how to pay for the order.
	pub payment: OrderPayment,
	/// Contains information about the channel state.
	pub channel: Option<ChannelInfo>,
}

/// An object representing the state of an order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum OrderState {
	/// The order has been created.
	Created,
	/// The LSP has opened the channel and published the funding transaction.
	Completed,
	/// The order failed.
	Failed,
}

/// Details regarding how to pay for an order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OrderPayment {
	/// Indicates the current state of the payment.
	pub state: PaymentState,
	/// The total fee the LSP will charge to open this channel in satoshi.
	#[serde(with = "string_amount")]
	pub fee_total_sat: u64,
	/// What the client needs to pay in total to open the requested channel.
	#[serde(with = "string_amount")]
	pub order_total_sat: u64,
	/// A BOLT11 invoice the client can pay to have to channel opened.
	pub bolt11_invoice: Bolt11Invoice,
	/// An on-chain address the client can send [`Self::order_total_sat`] to to have the channel
	/// opened.
	pub onchain_address: Address<NetworkUnchecked>,
	/// The minimum number of block confirmations that are required for the on-chain payment to be
	/// considered confirmed.
	pub min_onchain_payment_confirmations: Option<u8>,
	/// The minimum fee rate for the on-chain payment in case the client wants the payment to be
	/// confirmed without a confirmation.
	pub min_fee_for_0conf: u8,
	/// Details regarding a detected on-chain payment.
	pub onchain_payment: OnchainPayment,
}

/// The state of an [`OrderPayment`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum PaymentState {
	/// A payment is expected.
	ExpectPayment,
	/// A Lighting payment has arrived, but the preimage has not been released yet.
	Hold,
	/// A sufficient payment has been received.
	Paid,
	/// The payment has been refunded.
	Refunded,
}

/// Details regarding a detected on-chain payment.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OnchainPayment {
	/// The outpoint of the payment.
	pub outpoint: String,
	/// The amount of satoshi paid.
	#[serde(with = "string_amount")]
	pub sat: u64,
	/// Indicates if the LSP regards the transaction as sufficiently confirmed.
	pub confirmed: bool,
}

/// Details regarding the state of an ordered channel.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChannelInfo {
	/// The datetime when the funding transaction has been published.
	pub funded_at: String,
	/// The outpoint of the funding transaction.
	pub funding_outpoint: String,
	/// The earliest datetime when the channel may be closed by the LSP.
	pub expires_at: String,
}

/// A request made to an LSP to retrieve information about an previously made order.
///
/// Please refer to the [LSPS1 specification](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS1#21-lsps1get_order)
/// for more information.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetOrderRequest {
	/// The id of the order.
	pub order_id: OrderId,
}

/// An enum that captures all the valid JSON-RPC requests in the LSPS1 protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Request {
	/// A request to learn about the options supported by the LSP.
	GetInfo(GetInfoRequest),
	/// A request to create a channel order.
	CreateOrder(CreateOrderRequest),
	/// A request to query a previously created channel order.
	GetOrder(GetOrderRequest),
}

/// An enum that captures all the valid JSON-RPC responses in the LSPS1 protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Response {
	/// A successful response to a [`GetInfoRequest`].
	GetInfo(GetInfoResponse),
	/// An error response to a [`GetInfoRequest`].
	GetInfoError(ResponseError),
	/// A successful response to a [`CreateOrderRequest`].
	CreateOrder(CreateOrderResponse),
	/// An error response to a [`CreateOrderRequest`].
	CreateOrderError(ResponseError),
	/// A successful response to a [`GetOrderRequest`].
	GetOrder(CreateOrderResponse),
	/// An error response to a [`GetOrderRequest`].
	GetOrderError(ResponseError),
}

/// An enum that captures all valid JSON-RPC messages in the LSPS1 protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Message {
	/// An LSPS1 JSON-RPC request.
	Request(RequestId, LSPS1Request),
	/// An LSPS1 JSON-RPC response.
	Response(RequestId, LSPS1Response),
}

impl TryFrom<LSPSMessage> for LSPS1Message {
	type Error = ();

	fn try_from(message: LSPSMessage) -> Result<Self, Self::Error> {
		if let LSPSMessage::LSPS1(message) = message {
			return Ok(message);
		}

		Err(())
	}
}

impl From<LSPS1Message> for LSPSMessage {
	fn from(message: LSPS1Message) -> Self {
		LSPSMessage::LSPS1(message)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::alloc::string::ToString;

	#[test]
	fn options_supported_serialization() {
		let min_channel_confirmations = 6;
		let min_onchain_payment_confirmations = Some(6);
		let supports_zero_channel_reserve = true;
		let min_onchain_payment_size_sat = Some(100_000);
		let max_channel_expiry_blocks = 144;
		let min_initial_client_balance_sat = 10_000_000;
		let max_initial_client_balance_sat = 100_000_000;
		let min_initial_lsp_balance_sat = 100_000;
		let max_initial_lsp_balance_sat = 100_000_000;
		let min_channel_balance_sat = 100_000;
		let max_channel_balance_sat = 100_000_000;

		let options_supported = OptionsSupported {
			min_channel_confirmations,
			min_onchain_payment_confirmations,
			supports_zero_channel_reserve,
			min_onchain_payment_size_sat,
			max_channel_expiry_blocks,
			min_initial_client_balance_sat,
			max_initial_client_balance_sat,
			min_initial_lsp_balance_sat,
			max_initial_lsp_balance_sat,
			min_channel_balance_sat,
			max_channel_balance_sat,
		};

		let json_str = r#"{"max_channel_balance_sat":"100000000","max_channel_expiry_blocks":144,"max_initial_client_balance_sat":"100000000","max_initial_lsp_balance_sat":"100000000","min_channel_balance_sat":"100000","min_channel_confirmations":6,"min_initial_client_balance_sat":"10000000","min_initial_lsp_balance_sat":"100000","min_onchain_payment_confirmations":6,"min_onchain_payment_size_sat":"100000","supports_zero_channel_reserve":true}"#;
		assert_eq!(json_str, serde_json::json!(options_supported).to_string());
		assert_eq!(options_supported, serde_json::from_str(json_str).unwrap());
	}
}
