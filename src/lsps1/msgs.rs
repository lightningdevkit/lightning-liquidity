use chrono::Utc;
use std::convert::TryFrom;

use serde::{Deserialize, Serialize};

use crate::lsps0::msgs::{LSPSMessage, RequestId, ResponseError};

pub(crate) const LSPS1_GET_INFO_METHOD_NAME: &str = "lsps1.get_info";
pub(crate) const LSPS1_CREATE_ORDER_METHOD_NAME: &str = "lsps1.create_order";
pub(crate) const LSPS1_GET_ORDER_METHOD_NAME: &str = "lsps1.get_order";

pub(crate) const LSPS1_CREATE_ORDER_REQUEST_INVALID_PARAMS_ERROR_CODE: i32 = -32602;
pub(crate) const LSPS1_CREATE_ORDER_REQUEST_ORDER_MISMATCH_ERROR_CODE: i32 = 1000;
pub(crate) const LSPS1_CREATE_ORDER_REQUEST_CLIENT_REJECTED_ERROR_CODE: i32 = 1001;
pub(crate) const LSPS1_CREATE_ORDER_REQUEST_INVALID_VERSION_ERROR_CODE: i32 = 1;
pub(crate) const LSPS1_CREATE_ORDER_REQUEST_INVALID_TOKEN_ERROR_CODE: i32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Hash)]
pub struct OrderId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct GetInfoRequest {}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OptionsSupported {
	pub minimum_channel_confirmations: u8,
	pub minimum_onchain_payment_confirmations: u8,
	pub supports_zero_channel_reserve: bool,
	pub min_onchain_payment_size_sat: Option<u32>,
	pub max_channel_expiry_blocks: u32,
	pub min_initial_client_balance_sat: u64,
	pub max_initial_client_balance_sat: u64,
	pub min_initial_lsp_balance_sat: u64,
	pub max_initial_lsp_balance_sat: u64,
	pub min_channel_balance_sat: u64,
	pub max_channel_balance_sat: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetInfoResponse {
	pub supported_versions: Vec<u16>,
	pub website: String,
	pub options: OptionsSupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CreateOrderRequest {
	pub version: u16,
	pub order: Order,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Order {
	pub api_version: u16,
	pub lsp_balance_sat: u64,
	pub client_balance_sat: u64,
	pub confirms_within_blocks: u32,
	pub channel_expiry_blocks: u32,
	pub token: String,
	pub announce_channel: bool,
	pub refund_onchain_address: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CreateOrderResponse {
	pub order_id: OrderId,
	pub order: Order,
	pub order_state: OrderState,
	pub created_at: chrono::DateTime<Utc>,
	pub expires_at: chrono::DateTime<Utc>,
	pub payment: Payment,
	pub channel: Option<ChannelInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum OrderState {
	Requested,
	Created,
	Completed,
	Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Payment {
	pub state: PaymentState,
	pub fee_total_sat: u64,
	pub order_total_sat: u64,
	pub onchain_address: String,
	pub bolt11_invoice: String,
	pub onchain_block_confirmations_required: u8,
	pub minimum_fee_for_0conf: u8,
	pub onchain_payment: OnchainPayment,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum PaymentState {
	ExpectPayment,
	Hold,
	Paid,
	Refunded,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OnchainPayment {
	pub outpoint: String,
	pub sat: u64,
	pub confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChannelInfo {
	pub state: ChannelStatus,
	pub funded_at: String,
	pub funding_outpoint: String,
	pub scid: Option<String>,
	pub expires_at: String,
	pub closing_transaction: Option<String>,
	pub closed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum ChannelStatus {
	Opening,
	Opened,
	Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetOrderRequest {
	pub order_id: OrderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GetOrderResponse {
	pub response: CreateOrderResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Request {
	GetInfo(GetInfoRequest),
	CreateOrder(CreateOrderRequest),
	GetOrder(GetOrderRequest),
}

impl LSPS1Request {
	pub fn method(&self) -> &str {
		match self {
			LSPS1Request::GetInfo(_) => LSPS1_GET_INFO_METHOD_NAME,
			LSPS1Request::CreateOrder(_) => LSPS1_CREATE_ORDER_METHOD_NAME,
			LSPS1Request::GetOrder(_) => LSPS1_GET_ORDER_METHOD_NAME,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Response {
	GetInfo(GetInfoResponse),
	CreateOrder(CreateOrderResponse),
	CreateOrderError(ResponseError),
	GetOrder(GetOrderResponse),
	GetOrderError(ResponseError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1Message {
	Request(RequestId, LSPS1Request),
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
