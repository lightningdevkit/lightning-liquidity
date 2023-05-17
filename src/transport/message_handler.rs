use crate::events::{Event, EventHandler};
use crate::transport::msgs::{LSPSMessage, Prefix, RawLSPSMessage, LSPS_MESSAGE_TYPE};
use bitcoin::secp256k1::PublicKey;
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::CustomMessageReader;
use lightning::log_info;
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::io;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

/// A trait used to implement a specific LSPS protocol
/// The messages the protocol uses need to be able to be mapped
/// from and into LSPSMessages.
pub trait ProtocolMessageHandler {
	type ProtocolMessage: TryFrom<LSPSMessage> + Into<LSPSMessage>;
	const PROTOCOL_NUMBER: Option<u16>;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn get_and_clear_pending_protocol_messages(&self) -> Vec<(PublicKey, Self::ProtocolMessage)>;
	fn get_and_clear_pending_protocol_events(&self) -> Vec<Event>;
	fn get_protocol_number(&self) -> Option<u16> {
		Self::PROTOCOL_NUMBER
	}
}

/// A trait used to implement the mapping from a LSPS transport layer mesage
/// to a specific protocol message. This enables the ProtocolMessageHandler's
/// to not need to know about LSPSMessage and only have to deal with the specific
/// messages related to the protocol that is being implemented.
pub trait TransportMessageHandler {
	fn handle_lsps_message(
		&self, message: LSPSMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError>;
	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LSPSMessage)>;
	fn get_and_clear_pending_events(&self) -> Vec<Event>;
	fn get_protocol_number(&self) -> Option<u16>;
}

impl<T> TransportMessageHandler for T
where
	T: ProtocolMessageHandler,
	LSPSMessage: TryInto<<T as ProtocolMessageHandler>::ProtocolMessage>,
{
	fn handle_lsps_message(
		&self, message: LSPSMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		if let Ok(protocol_message) = message.try_into() {
			self.handle_message(protocol_message, counterparty_node_id)?;
		}

		Ok(())
	}

	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LSPSMessage)> {
		self.get_and_clear_pending_protocol_messages()
			.into_iter()
			.map(|(public_key, protocol_message)| (public_key, protocol_message.into()))
			.collect()
	}

	fn get_and_clear_pending_events(&self) -> Vec<Event> {
		self.get_and_clear_pending_protocol_events()
	}

	fn get_protocol_number(&self) -> Option<u16> {
		self.get_protocol_number()
	}
}

pub struct LSPManager<L: Deref>
where
	L::Target: Logger,
{
	logger: L,
	pending_messages: Mutex<Vec<(PublicKey, RawLSPSMessage)>>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	message_handlers: Arc<Mutex<HashMap<Prefix, Arc<dyn TransportMessageHandler>>>>,
}

impl<L: Deref> LSPManager<L>
where
	L::Target: Logger,
{
	pub fn new(logger: L) -> Self {
		Self {
			logger,
			pending_messages: Mutex::new(Vec::new()),
			request_id_to_method_map: Mutex::new(HashMap::new()),
			message_handlers: Arc::new(Mutex::new(HashMap::new())),
		}
	}

	pub fn get_message_handlers(
		&self,
	) -> Arc<Mutex<HashMap<Prefix, Arc<dyn TransportMessageHandler>>>> {
		self.message_handlers.clone()
	}

	pub fn register_message_handler(
		&self, prefix: Prefix, message_handler: Arc<dyn TransportMessageHandler>,
	) {
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
		&self, msg: LSPSMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		if let Some(prefix) = msg.prefix() {
			let message_handlers = self.message_handlers.lock().unwrap();
			// TODO: not sure what we are supposed to do when we receive a message we don't have a handler for
			if let Some(message_handler) = message_handlers.get(&prefix) {
				message_handler.handle_lsps_message(msg, sender_node_id)?;
			} else {
				log_info!(
					self.logger,
					"Received a message from {:?} we do not have a handler for: {:?}",
					sender_node_id,
					msg
				);
			}
		}
		Ok(())
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: RawLSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}
}

impl<L: Deref> CustomMessageReader for LSPManager<L>
where
	L::Target: Logger,
{
	type CustomMessage = RawLSPSMessage;

	fn read<R: io::Read>(
		&self, message_type: u16, buffer: &mut R,
	) -> Result<Option<Self::CustomMessage>, lightning::ln::msgs::DecodeError> {
		match message_type {
			LSPS_MESSAGE_TYPE => {
				let mut payload = String::new();
				buffer.read_to_string(&mut payload)?;
				Ok(Some(RawLSPSMessage { payload }))
			}
			_ => Ok(None),
		}
	}
}

impl<L: Deref> CustomMessageHandler for LSPManager<L>
where
	L::Target: Logger,
{
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();

		match LSPSMessage::from_str_with_id_map(&msg.payload, &mut request_id_to_method_map) {
			Ok(msg) => self.handle_lsps_message(msg, sender_node_id),
			Err(_) => {
				self.enqueue_message(
					*sender_node_id,
					RawLSPSMessage {
						payload: serde_json::to_string(&LSPSMessage::Invalid).unwrap(),
					},
				);
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
			msgs.extend(protocol_messages.into_iter().map(|(node_id, message)| {
				(node_id, RawLSPSMessage { payload: serde_json::to_string(&message).unwrap() })
			}));
		}

		msgs
	}
}
