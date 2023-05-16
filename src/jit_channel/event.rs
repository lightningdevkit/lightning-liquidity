// This file is Copyright its original authors, visible in version contror
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Events are returned from various bits in the library which indicate some action must be taken
//! by the client.
//!
//! Because we don't have a built-in runtime, it's up to the client to call events at a time in the
//! future, as well as generate and broadcast funding transactions handle payment preimages and a
//! few other things.

use bitcoin::secp256k1::PublicKey;

use super::msgs::OpeningFeeParams;
use crate::transport::msgs::RequestId;

/// An Event which you should probably take some action in response to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	GetInfo {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		version: u16,
		token: Option<String>,
	},
	GetInfoResponse {
		channel_id: u128,
		counterparty_node_id: PublicKey,
		opening_fee_params_menu: Vec<OpeningFeeParams>,
		min_payment_size_msat: u64,
		max_payment_size_msat: u64,
	},
	BuyRequest {
		request_id: RequestId,
		counterparty_node_id: PublicKey,
		version: u16,
		opening_fee_params: OpeningFeeParams,
		payment_size_msat: Option<u64>,
	},
	InvoiceGenerationReady {
		counterparty_node_id: PublicKey,
		scid: u64,
		cltv_expiry_delta: u32,
	},
}
