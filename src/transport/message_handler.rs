use crate::events::{Event, EventHandler};
use crate::transport::msgs;
use crate::transport::msgs::{LSPSMessage, Prefix};
use bitcoin::secp256k1::PublicKey;
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::CustomMessageReader;
use lightning::log_info;
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::io;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::Mutex;

use super::msgs::{LSPSNotification, LSPSRequest, LSPSResponse, RequestId, ResponseError};

pub trait ProtocolMessageHandler {
	type ProtocolRequest: TryFrom<LSPSRequest>;
	type ProtocolResponse: TryFrom<LSPSResponse>;
	type ProtocolNotification: TryFrom<LSPSNotification>;

	fn handle_lsps_request(
		&self, request_id: RequestId, request: Self::ProtocolRequest,
		counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn handle_lsps_response(
		&self, response: Self::ProtocolResponse, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn handle_lsps_notification(
		&self, notification: Self::ProtocolNotification, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LSPSMessage)>;
	fn get_and_clear_pending_events(&self) -> Vec<Event>;
	fn get_protocol_number(&self) -> Option<u16>;
}

pub struct LSPManager<PMH: Deref, L: Deref>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
{
	pending_messages: Mutex<Vec<(PublicKey, msgs::RawLSPSMessage)>>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	message_handlers: Arc<Mutex<HashMap<Prefix, PMH>>>,
	logger: L,
}

impl<PMH: Deref, L: Deref> LSPManager<PMH, L>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
{
	pub fn new(logger: L) -> Self {
		Self {
			pending_messages: Mutex::new(Vec::new()),
			request_id_to_method_map: Mutex::new(HashMap::new()),
			message_handlers: Arc::new(Mutex::new(HashMap::new())),
			logger,
		}
	}

	pub fn register_message_handler(&self, prefix: Prefix, message_handler: PMH) {
		self.message_handlers.lock().unwrap().insert(prefix, message_handler);
	}

	pub fn process_pending_events<H: EventHandler>(&self, handler: H) {
		let message_handlers = self.message_handlers.lock().unwrap();

		for message_handler in message_handlers.values() {
			let events = message_handler.get_and_clear_pending_events();
			for event in events {
				handler.handle_event(event);
			}
		}
	}

	fn handle_lsps_message(
		&self, msg: msgs::LSPSMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match msg {
			msgs::LSPSMessage::Request(request) => {
				let message_handlers = self.message_handlers.lock().unwrap();

				let request_id = RequestId(request.id().to_string());
				match message_handlers.get(&request.prefix()) {
					Some(message_handler) => {
						if let Ok(lsps_request) = request.try_into() {
							message_handler.handle_lsps_request(
								request_id,
								lsps_request,
								sender_node_id,
							)
						} else {
							Ok(())
						}
					}
					None => {
						// TODO: really should be something like unsupported request?
						self.send_invalid_request(sender_node_id);
						Ok(())
					}
				}
			}
			msgs::LSPSMessage::Response(response) => {
				let message_handlers = self.message_handlers.lock().unwrap();
				if let Some(prefix) = response.prefix() {
					if let Some(message_handler) = message_handlers.get(&prefix) {
						if let Ok(lsps_response) = response.try_into() {
							return message_handler
								.handle_lsps_response(lsps_response, sender_node_id);
						}
					}
				}
				Ok(())
			}
			msgs::LSPSMessage::Notification(notification) => {
				let message_handlers = self.message_handlers.lock().unwrap();

				match message_handlers.get(&notification.prefix()) {
					Some(message_handler) => {
						if let Ok(notification) = notification.try_into() {
							message_handler.handle_lsps_notification(notification, sender_node_id)
						} else {
							Ok(())
						}
					}
					None => {
						log_info!(
							self.logger,
							"Received notification for unknown protocol: {:?}",
							notification
						);
						Ok(())
					}
				}
			}
		}
	}

	fn send_invalid_request(&self, sender_node_id: &PublicKey) {
		let response = LSPSMessage::Response(LSPSResponse::InvalidRequest(ResponseError {
			code: -32167,
			message: "Invalid Request".to_string(),
			data: None,
		}));

		self.enqueue_message(
			*sender_node_id,
			msgs::RawLSPSMessage { payload: serde_json::to_string(&response).unwrap() },
		);
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: msgs::RawLSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}

	fn insert_pending_request(&self, request: LSPSRequest) {
		let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();
		request_id_to_method_map.insert(request.id().to_string(), request.method().to_string());
	}

	fn get_pending_request(&self, request_id: RequestId) -> Option<String> {
		let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();
		request_id_to_method_map.remove(&request_id.0)
	}
}

impl<PMH: Deref, L: Deref> CustomMessageReader for LSPManager<PMH, L>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
{
	type CustomMessage = msgs::RawLSPSMessage;

	fn read<R: io::Read>(
		&self, message_type: u16, buffer: &mut R,
	) -> Result<Option<Self::CustomMessage>, lightning::ln::msgs::DecodeError> {
		match message_type {
			msgs::LSPS_MESSAGE_TYPE => {
				let mut payload = String::new();
				buffer.read_to_string(&mut payload)?;
				Ok(Some(msgs::RawLSPSMessage { payload }))
			}
			_ => Ok(None),
		}
	}
}

impl<PMH: Deref, L: Deref> CustomMessageHandler for LSPManager<PMH, L>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
{
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();

		match LSPSMessage::from_str_with_id_map(&msg.payload, &request_id_to_method_map) {
			Ok(msg) => self.handle_lsps_message(msg, sender_node_id),
			Err(_) => {
				self.send_invalid_request(sender_node_id);
				Ok(())
			}
		}
	}

	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
		let mut msgs = vec![];

		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			msgs.extend(
				pending_messages.drain(..).collect::<Vec<(PublicKey, Self::CustomMessage)>>(),
			);
		}

		let message_handlers = self.message_handlers.lock().unwrap();
		for message_handler in message_handlers.values() {
			let protocol_messages = message_handler.get_and_clear_pending_msg();
			msgs.extend(protocol_messages.into_iter().map(|(node_id, lsp_message)| {
				(
					node_id,
					msgs::RawLSPSMessage { payload: serde_json::to_string(&lsp_message).unwrap() },
				)
			}));
		}

		msgs
	}
}
