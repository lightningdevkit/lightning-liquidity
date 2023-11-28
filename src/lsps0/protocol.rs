use crate::lsps0::message_handler::ProtocolMessageHandler;
use crate::lsps0::msgs::{
	LSPS0Message, LSPS0Request, LSPS0Response, LSPSMessage, ListProtocolsRequest,
	ListProtocolsResponse, RequestId, ResponseError,
};
use crate::prelude::Vec;
use crate::sync::{Arc, Mutex};
use crate::utils;

use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::sign::EntropySource;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

pub struct LSPS0MessageHandler<ES: Deref>
where
	ES::Target: EntropySource,
{
	entropy_source: ES,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	protocols: Vec<u16>,
}

impl<ES: Deref> LSPS0MessageHandler<ES>
where
	ES::Target: EntropySource,
{
	pub fn new(
		entropy_source: ES, protocols: Vec<u16>,
		pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	) -> Self {
		Self { entropy_source, protocols, pending_messages }
	}

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

	fn handle_request(
		&self, request_id: RequestId, request: LSPS0Request, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match request {
			LSPS0Request::ListProtocols(_) => {
				let msg = LSPS0Message::Response(
					request_id,
					LSPS0Response::ListProtocols(ListProtocolsResponse {
						protocols: self.protocols.clone(),
					}),
				);
				self.enqueue_message(*counterparty_node_id, msg);
				Ok(())
			}
		}
	}

	fn handle_response(
		&self, response: LSPS0Response, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match response {
			LSPS0Response::ListProtocols(ListProtocolsResponse { protocols }) => Ok(()),
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

impl<ES: Deref> ProtocolMessageHandler for LSPS0MessageHandler<ES>
where
	ES::Target: EntropySource,
{
	type ProtocolMessage = LSPS0Message;
	const PROTOCOL_NUMBER: Option<u16> = None;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS0Message::Request(request_id, request) => {
				self.handle_request(request_id, request, counterparty_node_id)
			}
			LSPS0Message::Response(_, response) => {
				self.handle_response(response, counterparty_node_id)
			}
		}
	}
}

#[cfg(test)]
mod tests {

	use alloc::string::ToString;
	use alloc::sync::Arc;

	use super::*;

	struct TestEntropy {}
	impl EntropySource for TestEntropy {
		fn get_secure_random_bytes(&self) -> [u8; 32] {
			[0; 32]
		}
	}

	#[test]
	fn test_handle_list_protocols_request() {
		let entropy = Arc::new(TestEntropy {});
		let protocols: Vec<u16> = vec![];
		let pending_messages = Arc::new(Mutex::new(vec![]));

		let lsps0_handler =
			Arc::new(LSPS0MessageHandler::new(entropy, protocols, pending_messages.clone()));

		let list_protocols_request = LSPS0Message::Request(
			RequestId("xyz123".to_string()),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		);
		let counterparty_node_id = utils::parse_pubkey(
			"027100442c3b79f606f80f322d98d499eefcb060599efc5d4ecb00209c2cb54190",
		)
		.unwrap();

		lsps0_handler.handle_message(list_protocols_request, &counterparty_node_id).unwrap();
		let pending_messages = pending_messages.lock().unwrap();

		assert_eq!(pending_messages.len(), 1);

		let (pubkey, message) = &pending_messages[0];

		assert_eq!(*pubkey, counterparty_node_id);
		assert_eq!(
			*message,
			LSPSMessage::LSPS0(LSPS0Message::Response(
				RequestId("xyz123".to_string()),
				LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: vec![] })
			))
		);
	}

	#[test]
	fn test_list_protocols() {
		let pending_messages = Arc::new(Mutex::new(vec![]));

		let lsps0_handler = Arc::new(LSPS0MessageHandler::new(
			Arc::new(TestEntropy {}),
			vec![1, 2, 3],
			pending_messages.clone(),
		));

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
