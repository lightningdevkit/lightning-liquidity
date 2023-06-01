use bitcoin::secp256k1::PublicKey;
use lightning::chain::keysinterface::EntropySource;
use lightning::{log_error, log_info};
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};

use crate::events::Event;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::utils;

use super::message_handler::TransportMessageHandler;
use super::msgs::{
	LSPS0Message, LSPS0Request, LSPS0Response, ListProtocolsRequest, ListProtocolsResponse, Prefix,
	RequestId, ResponseError,
};

pub struct LSPS0MessageHandler<L: Deref, ES: Deref>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	logger: L,
	message_handlers: Arc<RwLock<HashMap<Prefix, Arc<dyn TransportMessageHandler + Send + Sync>>>>,
	pending_messages: Mutex<Vec<(PublicKey, LSPS0Message)>>,
	entropy_source: ES,
	pending_events: Mutex<Vec<Event>>,
}

impl<L: Deref, ES: Deref> LSPS0MessageHandler<L, ES>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	pub fn new(
		logger: L, message_handlers: Arc<RwLock<HashMap<Prefix, Arc<dyn TransportMessageHandler + Send + Sync>>>>,
		entropy_source: ES,
	) -> Self {
		Self {
			logger,
			message_handlers,
			pending_messages: Mutex::new(vec![]),
			entropy_source,
			pending_events: Mutex::new(vec![]),
		}
	}

	pub fn generate_request_id(&self) -> RequestId {
		let bytes = self.entropy_source.get_secure_random_bytes();
		RequestId(utils::hex_str(&bytes[0..16]))
	}

	pub fn list_protocols(
		&self, counterparty_node_id: PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let msg = LSPS0Message::Request(
			self.generate_request_id(),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		);

		self.enqueue_message(counterparty_node_id, msg);

		Ok(())
	}

	fn enqueue_message(&self, counterparty_node_id: PublicKey, message: LSPS0Message) {
		self.pending_messages.lock().unwrap().push((counterparty_node_id, message));
	}

	fn enqueue_event(&self, event: Event) {
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.push(event);
	}

	fn handle_request(
		&self, request_id: RequestId, request: LSPS0Request, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match request {
			LSPS0Request::ListProtocols(_) => {
				let message_handlers = self.message_handlers.read().unwrap();
				let mut protocols = vec![];

				for message_handler in message_handlers.values() {
					if let Some(protocol_number) = message_handler.get_protocol_number() {
						protocols.push(protocol_number);
					}
				}

				let msg = LSPS0Message::Response(
					request_id,
					LSPS0Response::ListProtocols(ListProtocolsResponse { protocols }),
				);
				self.enqueue_message(*counterparty_node_id, msg);
				Ok(())
			}
		}
	}

	fn handle_response(
		&self, response: LSPS0Response, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match response {
			LSPS0Response::ListProtocols(ListProtocolsResponse { protocols }) => {
				self.enqueue_event(Event::ListProtocols {
					counterparty_node_id: *counterparty_node_id,
					protocols,
				});
				Ok(())
			}
			LSPS0Response::ListProtocolsError(ResponseError { code, message, data, .. }) => {
				log_error!(
					self.logger,
					"Received error({}) response to listprotocols with message: {} data: {:?}",
					code,
					message,
					data
				);
				Ok(())
			}
		}
	}
}

impl<L: Deref, ES: Deref> ProtocolMessageHandler for LSPS0MessageHandler<L, ES>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	type ProtocolMessage = LSPS0Message;
	const PROTOCOL_NUMBER: Option<u16> = None;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		log_info!(self.logger, "received transport protocol message: {:?}", message);
		match message {
			LSPS0Message::Request(request_id, request) => {
				self.handle_request(request_id, request, counterparty_node_id)
			}
			LSPS0Message::Response(_, response) => {
				self.handle_response(response, counterparty_node_id)
			}
		}
	}

	fn get_and_clear_pending_protocol_messages(
		&self,
	) -> Vec<(bitcoin::secp256k1::PublicKey, LSPS0Message)> {
		let mut pending_messages = self.pending_messages.lock().unwrap();
		std::mem::take(&mut *pending_messages)
	}

	fn get_and_clear_pending_protocol_events(&self) -> Vec<Event> {
		let mut pending_events = self.pending_events.lock().unwrap();
		std::mem::take(&mut *pending_events)
	}
}

#[cfg(test)]
mod tests {

	use super::*;

	struct TestLogger {}
	impl Logger for TestLogger {
		fn log(&self, _record: &lightning::util::logger::Record) {}
	}

	struct TestEntropy {}
	impl EntropySource for TestEntropy {
		fn get_secure_random_bytes(&self) -> [u8; 32] {
			[0; 32]
		}
	}

	#[test]
	fn test_handle_list_protocols_request() {
		let logger = Arc::new(TestLogger {});
		let entropy = Arc::new(TestEntropy {});
		let message_handlers: Arc<RwLock<HashMap<Prefix, Arc<dyn TransportMessageHandler + Send + Sync>>>> =
			Arc::new(RwLock::new(HashMap::new()));
		let lsps0_handler =
			Arc::new(LSPS0MessageHandler::new(logger, message_handlers.clone(), entropy));

		{
			let mut handlers = message_handlers.write().unwrap();
			handlers.insert(Prefix::LSPS0, lsps0_handler.clone());
		}

		let list_protocols_request = LSPS0Message::Request(
			RequestId("xyz123".to_string()),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		);
		let counterparty_node_id = utils::parse_pubkey(
			"027100442c3b79f606f80f322d98d499eefcb060599efc5d4ecb00209c2cb54190",
		)
		.unwrap();

		lsps0_handler.handle_message(list_protocols_request, &counterparty_node_id).unwrap();
		let pending_messages = lsps0_handler.get_and_clear_pending_protocol_messages();

		assert_eq!(pending_messages.len(), 1);

		let (pubkey, message) = &pending_messages[0];

		assert_eq!(*pubkey, counterparty_node_id);
		assert_eq!(
			*message,
			LSPS0Message::Response(
				RequestId("xyz123".to_string()),
				LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: vec![] })
			)
		);
	}

	#[test]
	fn test_list_protocols() {
		let lsps0_handler = Arc::new(LSPS0MessageHandler::new(
			Arc::new(TestLogger {}),
			Arc::new(RwLock::new(HashMap::new())),
			Arc::new(TestEntropy {}),
		));

		let counterparty_node_id = utils::parse_pubkey(
			"027100442c3b79f606f80f322d98d499eefcb060599efc5d4ecb00209c2cb54190",
		)
		.unwrap();

		lsps0_handler.list_protocols(counterparty_node_id).unwrap();
		let pending_messages = lsps0_handler.get_and_clear_pending_protocol_messages();

		assert_eq!(pending_messages.len(), 1);

		let (pubkey, message) = &pending_messages[0];

		assert_eq!(*pubkey, counterparty_node_id);
		assert_eq!(
			*message,
			LSPS0Message::Request(
				RequestId("00000000000000000000000000000000".to_string()),
				LSPS0Request::ListProtocols(ListProtocolsRequest {})
			)
		);
	}

	#[test]
	fn test_handle_list_protocols_response() {
		let lsps0_handler = Arc::new(LSPS0MessageHandler::new(
			Arc::new(TestLogger {}),
			Arc::new(RwLock::new(HashMap::new())),
			Arc::new(TestEntropy {}),
		));
		let list_protocols_response = LSPS0Message::Response(
			RequestId("00000000000000000000000000000000".to_string()),
			LSPS0Response::ListProtocols(ListProtocolsResponse { protocols: vec![1, 2, 3] }),
		);
		let counterparty_node_id = utils::parse_pubkey(
			"027100442c3b79f606f80f322d98d499eefcb060599efc5d4ecb00209c2cb54190",
		)
		.unwrap();

		lsps0_handler.handle_message(list_protocols_response, &counterparty_node_id).unwrap();
		let pending_events = lsps0_handler.get_and_clear_pending_protocol_events();

		assert_eq!(pending_events.len(), 1);
		assert_eq!(
			pending_events[0],
			Event::ListProtocols { counterparty_node_id, protocols: vec![1, 2, 3] }
		);
	}
}
