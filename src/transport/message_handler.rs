use crate::events::{Event, EventHandler};
use crate::transport::{jsonrpc, msgs};
use crate::utils;
use bitcoin::secp256k1::PublicKey;
use lightning::chain::keysinterface::EntropySource;
use lightning::{
	ln::{peer_handler::CustomMessageHandler, wire::CustomMessageReader},
	log_info,
	util::logger::Logger,
};
use std::{collections::HashMap, io, ops::Deref, sync::Mutex};

pub trait LSPMessageHandler {
	fn handle_lsps_request(
		&self, request: jsonrpc::Request, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn handle_lsps_response(
		&self, request: jsonrpc::Request, response: jsonrpc::Response,
		counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn handle_lsps_notification(
		&self, notification: jsonrpc::Notification, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, msgs::RawLSPSMessage)>;
	fn get_protocol_number(&self) -> u16;
}

pub struct LSPManager<LMH: Deref, L: Deref, ES: Deref>
where
	LMH::Target: LSPMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	pending_messages: Mutex<Vec<(PublicKey, msgs::RawLSPSMessage)>>,
	pending_requests: Mutex<HashMap<String, jsonrpc::Request>>,
	message_handlers: Mutex<HashMap<String, LMH>>,
	pending_events: Mutex<Vec<Event>>,
	logger: L,
	entropy_source: ES,
}

impl<LMH: Deref, L: Deref, ES: Deref> LSPManager<LMH, L, ES>
where
	LMH::Target: LSPMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	pub fn new(logger: L, entropy_source: ES) -> Self {
		Self {
			pending_messages: Mutex::new(Vec::new()),
			pending_requests: Mutex::new(HashMap::new()),
			message_handlers: Mutex::new(HashMap::new()),
			pending_events: Mutex::new(Vec::new()),
			logger,
			entropy_source,
		}
	}

	pub fn generate_request_id(&self) -> String {
		let bytes = self.entropy_source.get_secure_random_bytes();
		utils::hex_str(&bytes[0..16])
	}

	pub fn register_message_handler(&self, prefix: String, message_handler: LMH) {
		self.message_handlers.lock().unwrap().insert(prefix, message_handler);
	}

	pub fn list_protocols(
		&self, counterparty_node_id: PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let request = jsonrpc::Request {
			jsonrpc: jsonrpc::VERSION.to_string(),
			id: serde_json::Value::String(self.generate_request_id()),
			method: jsonrpc::Method {
				prefix: "lsps0".to_string(),
				name: "listprotocols".to_string(),
			},
			params: HashMap::new(),
		};

		let msg = msgs::RawLSPSMessage { payload: serde_json::to_string(&request).unwrap() };

		self.insert_pending_request(request);

		self.enqueue_message(counterparty_node_id, msg);
		Ok(())
	}

	pub fn process_pending_events<H: EventHandler>(&self, handler: H) {
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.drain(..).for_each(|event| handler.handle_event(event));
	}

	fn handle_lsps_message(
		&self, msg: msgs::LSPSMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match msg {
			msgs::LSPSMessage::Request(request) => {
				if request.method.prefix == "lsps0" && request.method.name == "listprotocols" {
					let mut protocols = vec![];
					let message_handlers = self.message_handlers.lock().unwrap();
					for message_handler in message_handlers.values() {
						protocols.push(message_handler.get_protocol_number());
					}

					let response = msgs::LSPSMessage::Response(jsonrpc::Response::Success(
						jsonrpc::SuccessResponse {
							id: request.id,
							result: protocols.into(),
							jsonrpc: jsonrpc::VERSION.to_string(),
						},
					));

					self.enqueue_message(
						*sender_node_id,
						msgs::RawLSPSMessage { payload: serde_json::to_string(&response).unwrap() },
					);
					return Ok(());
				}

				let message_handlers = self.message_handlers.lock().unwrap();

				match message_handlers.get(&request.method.prefix) {
					Some(message_handler) => {
						message_handler.handle_lsps_request(request, sender_node_id)
					}
					None => {
						// TODO: really should be something like unsupported request?
						self.send_invalid_request(sender_node_id);
						Ok(())
					}
				}
			}
			msgs::LSPSMessage::Response(response) => {
				match self.get_pending_request(response.get_id()) {
					Some(request) => {
						if request.method.prefix == "lsps0"
							&& request.method.name == "listprotocols"
						{
							match response {
								jsonrpc::Response::Success(success) => {
									if let Ok(protocols) =
										serde_json::from_value::<Vec<u16>>(success.result)
									{
										self.enqueue_event(Event::ListProtocols {
											protocols,
											counterparty_node_id: *sender_node_id,
										});
									}
								}
								jsonrpc::Response::Error(error) => {
									println!("Error: {:?}", error);
								}
							}
							return Ok(());
						}
						let message_handlers = self.message_handlers.lock().unwrap();

						match message_handlers.get(&request.method.prefix) {
							Some(message_handler) => message_handler.handle_lsps_response(
								request,
								response,
								sender_node_id,
							),
							None => {
								log_info!(self.logger, "Receive response for request we no longer have message handler for: {:?}", response);
								Ok(())
							}
						}
					}
					None => {
						log_info!(
							self.logger,
							"Received response for unknown request: {:?}",
							response
						);
						Ok(())
					}
				}
			}
			msgs::LSPSMessage::Notification(notification) => {
				let message_handlers = self.message_handlers.lock().unwrap();

				match message_handlers.get(&notification.method.prefix) {
					Some(message_handler) => {
						message_handler.handle_lsps_notification(notification, sender_node_id)
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
		let response =
			msgs::LSPSMessage::Response(jsonrpc::Response::Error(jsonrpc::ErrorResponse {
				jsonrpc: jsonrpc::VERSION.to_string(),
				error: jsonrpc::Error {
					code: jsonrpc::INVALID_REQUEST_ERROR_CODE,
					message: "Invalid Request".to_string(),
					data: None,
				},
				id: serde_json::Value::Null,
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

	fn enqueue_event(&self, event: Event) {
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.push(event);
	}

	fn insert_pending_request(&self, request: jsonrpc::Request) {
		let mut pending_requests = self.pending_requests.lock().unwrap();
		pending_requests.insert(request.id.to_string(), request);
	}

	fn get_pending_request(&self, request_id: &serde_json::Value) -> Option<jsonrpc::Request> {
		let mut pending_requests = self.pending_requests.lock().unwrap();
		pending_requests.remove(&request_id.to_string())
	}
}

impl<LMH: Deref, L: Deref, ES: Deref> CustomMessageReader for LSPManager<LMH, L, ES>
where
	LMH::Target: LSPMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
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

impl<LMH: Deref, L: Deref, ES: Deref> CustomMessageHandler for LSPManager<LMH, L, ES>
where
	LMH::Target: LSPMessageHandler,
	L::Target: Logger,
	ES::Target: EntropySource,
{
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match serde_json::from_str(&msg.payload) {
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
			let lsps_messages = message_handler.get_and_clear_pending_msg();
			msgs.extend(lsps_messages);
		}

		msgs
	}
}
