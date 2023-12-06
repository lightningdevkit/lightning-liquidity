// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

use super::msgs::OpeningFeeParams;
use crate::lsps0::msgs::RequestId;
use crate::prelude::{String, Vec};

use bitcoin::secp256k1::PublicKey;

/// An event which you should probably take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSPS2Event {
	/// A request from a client for information about JIT Channel parameters.
	///
	/// You must calculate the parameters for this client and pass them to
	/// [`LiquidityManager::opening_fee_params_generated`].
	///
	/// If an unrecognized or stale token is provided you can use
	/// `[LiquidityManager::invalid_token_provided`] to error the request.
	///
	/// [`LiquidityManager::opening_fee_params_generated`]: crate::lsps0::message_handler::LiquidityManager::opening_fee_params_generated
	/// [`LiquidityManager::invalid_token_provided`]: crate::lsps0::message_handler::LiquidityManager::invalid_token_provided
	GetInfo {
		/// An identifier that must be passed to [`LiquidityManager::opening_fee_params_generated`].
		///
		/// [`LiquidityManager::opening_fee_params_generated`]: crate::lsps0::message_handler::LiquidityManager::opening_fee_params_generated
		request_id: RequestId,
		/// The node id of the client making the information request.
		counterparty_node_id: PublicKey,
		/// The protocol version they would like to use.
		version: u16,
		/// An optional token that can be used as an API key, coupon code, etc.
		token: Option<String>,
	},
	/// Information from the LSP about their current fee rates and channel parameters.
	///
	/// You must call [`LiquidityManager::opening_fee_params_selected`] with the fee parameter
	/// you want to use if you wish to proceed opening a channel.
	///
	/// [`LiquidityManager::opening_fee_params_selected`]: crate::lsps0::message_handler::LiquidityManager::opening_fee_params_selected
	GetInfoResponse {
		/// This is a randomly generated identifier used to track the JIT channel state.
		/// It is not related in anyway to the eventual lightning channel id.
		/// It needs to be passed to [`LiquidityManager::opening_fee_params_selected`].
		///
		/// [`LiquidityManager::opening_fee_params_selected`]: crate::lsps0::message_handler::LiquidityManager::opening_fee_params_selected
		jit_channel_id: u128,
		/// The node id of the LSP that provided this response.
		counterparty_node_id: PublicKey,
		/// The menu of fee parameters the LSP is offering at this time.
		/// You must select one of these if you wish to proceed.
		opening_fee_params_menu: Vec<OpeningFeeParams>,
		/// The min payment size allowed when opening the channel.
		min_payment_size_msat: u64,
		/// The max payment size allowed when opening the channel.
		max_payment_size_msat: u64,
		/// The user_channel_id value passed in to [`LiquidityManager::lsps2_create_invoice`].
		///
		/// [`LiquidityManager::lsps2_create_invoice`]: crate::lsps0::message_handler::LiquidityManager::lsps2_create_invoice
		user_channel_id: u128,
	},
	/// A client has selected a opening fee parameter to use and would like to
	/// purchase a channel with an optional initial payment size.
	///
	/// If `payment_size_msat` is [`Option::Some`] then the payer is allowed to use MPP.
	/// If `payment_size_msat` is [`Option::None`] then the payer cannot use MPP.
	///
	/// You must generate an scid and `cltv_expiry_delta` for them to use
	/// and call [`LiquidityManager::invoice_parameters_generated`].
	///
	/// [`LiquidityManager::invoice_parameters_generated`]: crate::lsps0::message_handler::LiquidityManager::invoice_parameters_generated
	BuyRequest {
		/// An identifier that must be passed into [`LiquidityManager::invoice_parameters_generated`].
		///
		/// [`LiquidityManager::invoice_parameters_generated`]: crate::lsps0::message_handler::LiquidityManager::invoice_parameters_generated
		request_id: RequestId,
		/// The client node id that is making this request.
		counterparty_node_id: PublicKey,
		/// The version of the protocol they would like to use.
		version: u16,
		/// The channel parameters they have selected.
		opening_fee_params: OpeningFeeParams,
		/// The size of the initial payment they would like to receive.
		payment_size_msat: Option<u64>,
	},
	/// Use the provided fields to generate an invoice and give to payer.
	///
	/// When the invoice is paid the LSP will open a channel to you
	/// with the previously agreed upon parameters.
	InvoiceGenerationReady {
		/// The node id of the LSP.
		counterparty_node_id: PublicKey,
		/// The short channel id to use in the route hint.
		scid: u64,
		/// The `cltv_expiry_delta` to use in the route hint.
		cltv_expiry_delta: u32,
		/// The initial payment size you specified.
		payment_size_msat: Option<u64>,
		/// The trust model the LSP expects.
		client_trusts_lsp: bool,
		/// The `user_channel_id` value passed in to [`LiquidityManager::lsps2_create_invoice`].
		///
		/// [`LiquidityManager::lsps2_create_invoice`]: crate::lsps0::message_handler::LiquidityManager::lsps2_create_invoice
		user_channel_id: u128,
	},
	/// You should open a channel using [`ChannelManager::create_channel`].
	///
	/// [`ChannelManager::create_channel`]: lightning::ln::channelmanager::ChannelManager::create_channel
	OpenChannel {
		/// The node to open channel with.
		their_network_key: PublicKey,
		/// The amount to forward after fees.
		amt_to_forward_msat: u64,
		/// The fee earned for opening the channel.
		opening_fee_msat: u64,
		/// An internal id used to track channel open.
		user_channel_id: u128,
	},
}
