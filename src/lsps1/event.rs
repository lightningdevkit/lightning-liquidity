// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Contains LSPS1 event types

use super::msgs::{ChannelInfo, OptionsSupported, OrderId, OrderParams, OrderPayment};

use crate::lsps0::msgs::RequestId;
use crate::prelude::String;

use bitcoin::secp256k1::PublicKey;

/// An event which an LSPS1 client should take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1ClientEvent {
	/// Information from the LSP about their supported protocol options.
	///
	/// You must check whether LSP supports the parameters the client wants and then call
	/// [`LSPS1ClientHandler::place_order`] to place an order.
	///
	/// [`LSPS1ClientHandler::place_order`]: crate::lsps1::client::LSPS1ClientHandler::place_order
	GetInfoResponse {
		/// This is a randomly generated identifier used to track the channel state.
		///
		/// It is not related in anyway to the eventual lightning channel id.
		/// It needs to be passed to [`LSPS1ClientHandler::place_order`].
		///
		/// [`LSPS1ClientHandler::place_order`]: crate::lsps1::client::LSPS1ClientHandler::place_order
		id: u128,
		/// An identifier to track messages received.
		request_id: RequestId,
		/// The node id of the LSP that provided this response.
		counterparty_node_id: PublicKey,
		/// The website of the LSP.
		website: String,
		/// All options supported by the LSP.
		options_supported: OptionsSupported,
	},
	/// Confirmation from the LSP about the order created by the client.
	///
	/// When the payment is confirmed, the LSP will open a channel to you
	/// with the below agreed upon parameters.
	///
	/// You must pay the invoice if you want to continue and then
	/// call [`LSPS1ClientHandler::check_order_status`] with the order id
	/// to get information from LSP about progress of the order.
	///
	/// [`LSPS1ClientHandler::check_order_status`]: crate::lsps1::client::LSPS1ClientHandler::check_order_status
	DisplayOrder {
		/// This is a randomly generated identifier used to track the channel state.
		/// It is not related in anyway to the eventual lightning channel id.
		/// It needs to be passed to [`LSPS1ClientHandler::check_order_status`].
		///
		/// [`LSPS1ClientHandler::check_order_status`]: crate::lsps1::client::LSPS1ClientHandler::check_order_status
		id: u128,
		/// The node id of the LSP.
		counterparty_node_id: PublicKey,
		/// The order created by client and approved by LSP.
		order: OrderParams,
		/// The details regarding payment of the order
		payment: OrderPayment,
		/// The details regarding state of the channel ordered.
		channel: Option<ChannelInfo>,
	},
}

/// An event which an LSPS1 server should take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS1ServiceEvent {
	/// A client has selected the parameters to use from the supported options of the LSP
	/// and would like to open a channel with the given payment parameters.
	///
	/// You must call [`LSPS1ServiceHandler::send_invoice_for_order`] to
	/// generate a complete invoice including the details regarding the
	/// payment and order id for this order for the client.
	///
	/// [`LSPS1ServiceHandler::send_invoice_for_order`]: crate::lsps1::service::LSPS1ServiceHandler::send_invoice_for_order
	CreateInvoice {
		/// An identifier that must be passed to [`LSPS1ServiceHandler::send_invoice_for_order`].
		///
		/// [`LSPS1ServiceHandler::send_invoice_for_order`]: crate::lsps1::service::LSPS1ServiceHandler::send_invoice_for_order
		request_id: RequestId,
		/// The node id of the client making the information request.
		counterparty_node_id: PublicKey,
		/// The order requested by the client.
		order: OrderParams,
	},
	/// A request from client to check the status of the payment.
	///
	/// An event to poll for checking payment status either onchain or lightning.
	///
	/// You must call [`LSPS1ServiceHandler::update_order_status`] to update the client
	/// regarding the status of the payment and order.
	///
	/// [`LSPS1ServiceHandler::update_order_status`]: crate::lsps1::service::LSPS1ServiceHandler::update_order_status
	CheckPaymentConfirmation {
		/// An identifier that must be passed to [`LSPS1ServiceHandler::update_order_status`].
		///
		/// [`LSPS1ServiceHandler::update_order_status`]: crate::lsps1::service::LSPS1ServiceHandler::update_order_status
		request_id: RequestId,
		/// The node id of the client making the information request.
		counterparty_node_id: PublicKey,
		/// The order id of order with pending payment.
		order_id: OrderId,
	},
	/// If error is encountered, refund the amount if paid by the client.
	Refund {
		/// An identifier.
		request_id: RequestId,
		/// The node id of the client making the information request.
		counterparty_node_id: PublicKey,
		/// The order id of the refunded order.
		order_id: OrderId,
	},
}
