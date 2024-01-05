//! Contains LSPS1 event types

use super::msgs::{ChannelInfo, OptionsSupported, OrderId, OrderParams, OrderPayment};

use crate::lsps0::msgs::RequestId;
use crate::prelude::String;

use bitcoin::secp256k1::PublicKey;

/// An event which an LSPS1 client should take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1ClientEvent {
	/// TODO
	GetInfoResponse {
		/// TODO
		id: u128,
		/// TODO
		request_id: RequestId,
		/// TODO
		counterparty_node_id: PublicKey,
		/// TODO
		website: String,
		/// TODO
		options_supported: OptionsSupported,
	},
	/// TODO
	DisplayOrder {
		/// TODO
		id: u128,
		/// TODO
		counterparty_node_id: PublicKey,
		/// TODO
		order: OrderParams,
		/// TODO
		payment: OrderPayment,
		/// TODO
		channel: Option<ChannelInfo>,
	},
}

/// An event which an LSPS1 server should take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1ServiceEvent {
	/// TODO
	CreateInvoice {
		/// TODO
		request_id: RequestId,
		/// TODO
		counterparty_node_id: PublicKey,
		/// TODO
		order: OrderParams,
	},
	/// TODO
	CheckPaymentConfirmation {
		/// TODO
		request_id: RequestId,
		/// TODO
		counterparty_node_id: PublicKey,
		/// TODO
		order_id: OrderId,
	},
	/// TODO
	Refund {
		/// TODO
		request_id: RequestId,
		/// TODO
		counterparty_node_id: PublicKey,
		/// TODO
		order_id: OrderId,
	},
}
