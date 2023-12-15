//! Contains the main LSPS2 client-side object, [`LSPS0ClientHandler`].
//!
//! Please refer to the [LSPS0
//! specifcation](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0) for more
//! information.

use crate::lsps0::msgs::{
	LSPS0Message, LSPS0Request, LSPS0Response, LSPSMessage, ListProtocolsRequest,
	ListProtocolsResponse, ProtocolMessageHandler, ResponseError,
};
use crate::prelude::Vec;
use crate::sync::{Arc, Mutex};
use crate::utils;

use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::sign::EntropySource;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

/// A message handler capable of sending and handling LSPS0 messages.
pub struct LSPS0ClientHandler<ES: Deref>
where
	ES::Target: EntropySource,
{
	entropy_source: ES,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
}

impl<ES: Deref> LSPS0ClientHandler<ES>
where
	ES::Target: EntropySource,
{
	/// Returns a new instance of [`LSPS0ClientHandler`].
	pub(crate) fn new(
		entropy_source: ES, pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	) -> Self {
		Self { entropy_source, pending_messages }
	}

	/// Calls LSPS0's `list_protocols`.
	///
	/// Please refer to the [LSPS0
	/// specifcation](https://github.com/BitcoinAndLightningLayerSpecs/lsp/tree/main/LSPS0#lsps-specification-support-query)
	/// for more information.
	pub fn list_protocols(&self, counterparty_node_id: PublicKey) {
		let msg = LSPS0Message::Request(
			utils::generate_request_id(&self.entropy_source),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		);

		self.enqueue_message(counterparty_node_id, msg);
	}

	fn enqueue_message(&self, counterparty_node_id: PublicKey, message: LSPS0Message) {
		self.pending_messages.lock().unwrap().push((counterparty_node_id, message.into()));
	}

	fn handle_response(
		&self, response: LSPS0Response, _counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match response {
			LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: _ }) => Ok(()),
			LSPS0Response::ListProtocolsError(ResponseError { code, message, data, .. }) => {
				Err(LightningError {
					err: format!(
						"ListProtocols error received. code = {}, message = {}, data = {:?}",
						code, message, data
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})
			}
		}
	}
}

impl<ES: Deref> ProtocolMessageHandler for LSPS0ClientHandler<ES>
where
	ES::Target: EntropySource,
{
	type ProtocolMessage = LSPS0Message;
	const PROTOCOL_NUMBER: Option<u16> = None;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS0Message::Response(_, response) => {
				self.handle_response(response, counterparty_node_id)
			}
			LSPS0Message::Request(..) => {
				debug_assert!(
					false,
					"Client handler received LSPS0 request message. This should never happen."
				);
				Err(LightningError { err: format!("Client handler received LSPS0 request message from node {:?}. This should never happen.", counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)})
			}
		}
	}
}

#[cfg(test)]
mod tests {

	use alloc::string::ToString;
	use alloc::sync::Arc;

	use crate::lsps0::msgs::RequestId;

	use super::*;

	struct TestEntropy {}
	impl EntropySource for TestEntropy {
		fn get_secure_random_bytes(&self) -> [u8; 32] {
			[0; 32]
		}
	}

	#[test]
	fn test_list_protocols() {
		let pending_messages = Arc::new(Mutex::new(vec![]));

		let lsps0_handler =
			Arc::new(LSPS0ClientHandler::new(Arc::new(TestEntropy {}), pending_messages.clone()));

		let counterparty_node_id = utils::parse_pubkey(
			"027100442c3b79f606f80f322d98d499eefcb060599efc5d4ecb00209c2cb54190",
		)
		.unwrap();

		lsps0_handler.list_protocols(counterparty_node_id);
		let pending_messages = pending_messages.lock().unwrap();

		assert_eq!(pending_messages.len(), 1);

		let (pubkey, message) = &pending_messages[0];

		assert_eq!(*pubkey, counterparty_node_id);
		assert_eq!(
			*message,
			LSPSMessage::LSPS0(LSPS0Message::Request(
				RequestId("00000000000000000000000000000000".to_string()),
				LSPS0Request::ListProtocols(ListProtocolsRequest {})
			))
		);
	}
}
