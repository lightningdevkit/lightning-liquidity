use bitcoin::secp256k1::PublicKey;

use super::msgs::{ChannelInfo, OptionsSupported, Order, OrderId, Payment};
use crate::transport::msgs::RequestId;

/// An Event which you should probably take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	GetInfoResponse {
		request_id: RequestId,

		counterparty_node_id: PublicKey,

		version: Vec<u16>,

		website: String,

		options_supported: OptionsSupported,
	},

	CreateInvoice {
		request_id: RequestId,

		counterparty_node_id: PublicKey,

		order: Order,

		order_id: OrderId,
	},

	DisplayOrder {
		request_id: RequestId,

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

	OpenChannel {},

	Refund {},
}
