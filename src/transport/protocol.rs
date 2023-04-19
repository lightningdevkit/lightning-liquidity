use bitcoin::secp256k1::PublicKey;
use lightning::chain::keysinterface::EntropySource;
use lightning::log_error;
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use crate::events::Event;
use crate::transport;
use crate::transport::jsonrpc;
use crate::transport::jsonrpc::Prefix;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::transport::msgs::LSPSMessage;
use crate::utils;

pub enum LSPS0Message {
	Request(LSPS0Request),
	Response(LSPS0Response),
	Notification(LSPS0Notification),
}

impl From<LSPS0Message> for LSPSMessage {
	fn from(message: LSPS0Message) -> Self {
		match message {
			LSPS0Message::Request(request) => request.into(),
			LSPS0Message::Response(response) => response.into(),
			LSPS0Message::Notification(notification) => notification.into(),
		}
	}
}

pub enum LSPS0Request {
	ListProtocols { request_id: String },
}

impl TryFrom<jsonrpc::Request> for LSPS0Request {
	type Error = ();

	fn try_from(request: jsonrpc::Request) -> Result<Self, Self::Error> {
		match request.method.name.as_str() {
			"listprotocols" => Ok(Self::ListProtocols { request_id: request.id.to_string() }),
			_ => Err(()),
		}
	}
}

impl From<LSPS0Request> for LSPSMessage {
	fn from(request: LSPS0Request) -> Self {
		match request {
			LSPS0Request::ListProtocols { request_id } => LSPSMessage::Request(jsonrpc::Request {
				method: jsonrpc::Method {
					name: "listprotocols".to_string(),
					prefix: Prefix::LSPS0,
				},
				params: HashMap::new(),
				id: request_id.into(),
				jsonrpc: transport::jsonrpc::VERSION.to_string(),
			}),
		}
	}
}

pub enum LSPS0Response {
	ListProtocolsSuccess { request_id: String, protocols: Vec<u16> },
	ListProtocolsError { request_id: String, code: i32, message: String, data: Option<String> },
}

impl TryFrom<(jsonrpc::Request, jsonrpc::Response)> for LSPS0Response {
	type Error = ();

	fn try_from(
		(request, response): (jsonrpc::Request, jsonrpc::Response),
	) -> Result<Self, Self::Error> {
		match request.method.name.as_str() {
			"listprotocols" => match response {
				jsonrpc::Response::Success(response) => match response.result {
					serde_json::Value::Array(values) => {
						let mut protocols = vec![];

						for value in values {
							if let serde_json::Value::Number(protocol) = value {
								protocols.push(protocol.as_u64().unwrap() as u16);
							} else {
								return Err(());
							}
						}

						Ok(Self::ListProtocolsSuccess {
							request_id: request.id.to_string(),
							protocols,
						})
					}
					_ => Err(()),
				},
				jsonrpc::Response::Error(response) => Ok(Self::ListProtocolsError {
					request_id: request.id.to_string(),
					code: response.error.code,
					message: response.error.message,
					data: response.error.data.map(|d| d.to_string()),
				}),
			},
			_ => Err(()),
		}
	}
}

impl From<LSPS0Response> for LSPSMessage {
	fn from(response: LSPS0Response) -> Self {
		match response {
			LSPS0Response::ListProtocolsSuccess { request_id, protocols } => {
				LSPSMessage::Response(jsonrpc::Response::Success(jsonrpc::SuccessResponse {
					result: serde_json::Value::Array(
						protocols
							.into_iter()
							.map(|p| serde_json::Value::Number(serde_json::Number::from(p as u64)))
							.collect(),
					),
					id: request_id.into(),
					jsonrpc: transport::jsonrpc::VERSION.to_string(),
				}))
			}
			LSPS0Response::ListProtocolsError { request_id, code, message, data } => {
				LSPSMessage::Response(jsonrpc::Response::Error(jsonrpc::ErrorResponse {
					error: jsonrpc::Error {
						code,
						message,
						data: data.map(serde_json::Value::String),
					},
					id: request_id.into(),
					jsonrpc: transport::jsonrpc::VERSION.to_string(),
				}))
			}
		}
	}
}

pub enum LSPS0Notification {
	Unknown,
}

impl From<jsonrpc::Notification> for LSPS0Notification {
	fn from(_notification: jsonrpc::Notification) -> Self {
		Self::Unknown
	}
}

impl From<LSPS0Notification> for LSPSMessage {
	fn from(notification: LSPS0Notification) -> Self {
		match notification {
			LSPS0Notification::Unknown => LSPSMessage::Notification(jsonrpc::Notification {
				method: jsonrpc::Method { name: "unknown".to_string(), prefix: Prefix::LSPS0 },
				params: HashMap::new(),
				jsonrpc: transport::jsonrpc::VERSION.to_string(),
			}),
		}
	}
}

pub struct LSPS0MessageHandler<PMH: Deref, L: Deref, ES: Deref>
where
	PMH::Target: ProtocolMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	logger: L,
	message_handlers: Arc<Mutex<HashMap<Prefix, PMH>>>,
	pending_messages: Mutex<Vec<(PublicKey, LSPS0Message)>>,
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

	pub fn generate_request_id(&self) -> String {
		let bytes = self.entropy_source.get_secure_random_bytes();
		utils::hex_str(&bytes[0..16])
	}

	pub fn list_protocols(
		&self, counterparty_node_id: PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		self.enqueue_message(
			counterparty_node_id,
			LSPS0Message::Request(LSPS0Request::ListProtocols {
				request_id: self.generate_request_id(),
			}),
		);
		Ok(())
	}

	fn enqueue_message(&self, counterparty_node_id: PublicKey, message: LSPS0Message) {
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
	type ProtocolMessage = LSPS0Message;

	fn handle_lsps_request(
		&self, request: Self::ProtocolRequest, counterparty_node_id: &bitcoin::secp256k1::PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match request {
			LSPS0Request::ListProtocols { request_id } => {
				let message_handlers = self.message_handlers.lock().unwrap();
				let mut protocols = vec![];

				for message_handler in message_handlers.values() {
					if let Some(protocol_number) = message_handler.get_protocol_number() {
						protocols.push(protocol_number);
					}
				}

				self.enqueue_message(
					*counterparty_node_id,
					LSPS0Message::Response(LSPS0Response::ListProtocolsSuccess {
						request_id,
						protocols,
					}),
				);
				Ok(())
			}
		}
	}

	fn handle_lsps_response(
		&self, response: Self::ProtocolResponse,
		counterparty_node_id: &bitcoin::secp256k1::PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match response {
			LSPS0Response::ListProtocolsSuccess { protocols, .. } => {
				self.enqueue_event(Event::ListProtocols {
					counterparty_node_id: *counterparty_node_id,
					protocols,
				});
				Ok(())
			}
			LSPS0Response::ListProtocolsError { code, message, data, .. } => {
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

	fn get_and_clear_pending_msg(&self) -> Vec<(bitcoin::secp256k1::PublicKey, LSPS0Message)> {
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
