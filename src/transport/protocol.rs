use bitcoin::secp256k1::PublicKey;
use lightning::chain::keysinterface::EntropySource;
use lightning::log_error;
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use crate::events::Event;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::transport::msgs::LSPSMessage;
use crate::utils;

use super::msgs::{
	LSPS0Notification, LSPS0Request, LSPS0Response, LSPSRequest, LSPSResponse,
	ListProtocolsRequest, ListProtocolsResponse, Prefix, RequestId, ResponseError,
};

pub struct LSPS0MessageHandler<PMH: Deref, L: Deref, ES: Deref>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	logger: L,
	message_handlers: Arc<Mutex<HashMap<Prefix, PMH>>>,
	pending_messages: Mutex<Vec<(PublicKey, LSPSMessage)>>,
	entropy_source: ES,
	pending_events: Mutex<Vec<Event>>,
}

impl<PMH: Deref, L: Deref, ES: Deref> LSPS0MessageHandler<PMH, L, ES>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	pub fn new(
		logger: L, message_handlers: Arc<Mutex<HashMap<Prefix, PMH>>>, entropy_source: ES,
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
		let msg = LSPSMessage::Request(LSPSRequest::LSPS0(
			self.generate_request_id(),
			LSPS0Request::ListProtocols(ListProtocolsRequest {}),
		));

		self.enqueue_message(counterparty_node_id, msg);

		Ok(())
	}

	fn enqueue_message(&self, counterparty_node_id: PublicKey, message: LSPSMessage) {
		self.pending_messages.lock().unwrap().push((counterparty_node_id, message));
	}

	fn enqueue_event(&self, event: Event) {
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.push(event);
	}
}

impl<PMH: Deref, L: Deref, ES: Deref> ProtocolMessageHandler for LSPS0MessageHandler<PMH, L, ES>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	type ProtocolRequest = LSPS0Request;
	type ProtocolResponse = LSPS0Response;
	type ProtocolNotification = LSPS0Notification;

	fn handle_lsps_request(
		&self, request_id: RequestId, request: Self::ProtocolRequest,
		counterparty_node_id: &bitcoin::secp256k1::PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match request {
			LSPS0Request::ListProtocols(_) => {
				let message_handlers = self.message_handlers.lock().unwrap();
				let mut protocols = vec![];

				for message_handler in message_handlers.values() {
					if let Some(protocol_number) = message_handler.get_protocol_number() {
						protocols.push(protocol_number);
					}
				}

				let response = LSPS0Response::ListProtocols(ListProtocolsResponse { protocols });
				let msg = LSPSMessage::Response(LSPSResponse::LSPS0(request_id, response));
				self.enqueue_message(*counterparty_node_id, msg);
				Ok(())
			}
		}
	}

	fn handle_lsps_response(
		&self, response: Self::ProtocolResponse,
		counterparty_node_id: &bitcoin::secp256k1::PublicKey,
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

	fn handle_lsps_notification(
		&self, _notification: Self::ProtocolNotification,
		_counterparty_node_id: &bitcoin::secp256k1::PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		Ok(())
	}

	fn get_and_clear_pending_msg(&self) -> Vec<(bitcoin::secp256k1::PublicKey, LSPSMessage)> {
		let mut pending_messages = self.pending_messages.lock().unwrap();
		std::mem::take(&mut *pending_messages)
	}

	fn get_and_clear_pending_events(&self) -> Vec<Event> {
		let mut pending_events = self.pending_events.lock().unwrap();
		std::mem::take(&mut *pending_events)
	}

	fn get_protocol_number(&self) -> Option<u16> {
		None
	}
}
