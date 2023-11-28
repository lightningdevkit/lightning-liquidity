#![allow(missing_docs)]

use super::msgs::{ChannelInfo, OptionsSupported, Order, OrderId, Payment};

use crate::lsps0::msgs::RequestId;
use crate::prelude::String;

use bitcoin::secp256k1::PublicKey;

/// An "Event" which you should probably take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	GetInfoResponse {
		id: u128,
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		version: u16,
		website: String,
		options_supported: OptionsSupported,
	},

	CreateInvoice {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		order: Order,
	},

	DisplayOrder {
		id: u128,
		counterparty_node_id: PublicKey,
		order: Order,
		payment: Payment,
		channel: Option<ChannelInfo>,
	},

	PayforChannel {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		order: Order,
		payment: Payment,
		channel: Option<ChannelInfo>,
	},

	CheckPaymentConfirmation {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		order_id: OrderId,
	},

	OpenChannel {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		order_id: OrderId,
	},

	Refund {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		order_id: OrderId,
	},
}
