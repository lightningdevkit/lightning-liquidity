use crate::transport::msgs::{LSPSMessage, RawLSPSMessage, LSPS_MESSAGE_TYPE};
use crate::transport::protocol::LSPS0MessageHandler;

use bitcoin::secp256k1::PublicKey;
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::CustomMessageReader;
use lightning::sign::EntropySource;
use lightning::util::logger::Level;
use lightning::util::ser::Readable;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

const LSPS_FEATURE_BIT: usize = 729;

/// A trait used to implement a specific LSPS protocol.
///
/// The messages the protocol uses need to be able to be mapped
/// from and into [`LSPSMessage`].
pub(crate) trait ProtocolMessageHandler {
	type ProtocolMessage: TryFrom<LSPSMessage> + Into<LSPSMessage>;
	const PROTOCOL_NUMBER: Option<u16>;

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError>;
}

/// A configuration for [`LiquidityManager`].
///
/// Allows end-user to configure options when using the [`LiquidityManager`]
/// to provide liquidity services to clients.
pub struct LiquidityProviderConfig;

/// The main interface into LSP functionality.
///
/// Should be used as a [`CustomMessageHandler`] for your
/// [`lightning::ln::peer_handler::PeerManager`]'s [`lightning::ln::peer_handler::MessageHandler`].
pub struct LiquidityManager<ES: Deref>
where
	ES::Target: EntropySource,
{
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	lsps0_message_handler: LSPS0MessageHandler<ES>,
	provider_config: Option<LiquidityProviderConfig>,
}

impl<ES: Deref> LiquidityManager<ES>
where
	ES::Target: EntropySource,
{
	/// Constructor for the LiquidityManager
	///
	/// Sets up the required protocol message handlers based on the given [`LiquidityProviderConfig`].
	pub fn new(entropy_source: ES, provider_config: Option<LiquidityProviderConfig>) -> Self {
		let pending_messages = Arc::new(Mutex::new(vec![]));

		let lsps0_message_handler =
			LSPS0MessageHandler::new(entropy_source, vec![], Arc::clone(&pending_messages));

		Self {
			pending_messages,
			request_id_to_method_map: Mutex::new(HashMap::new()),
			lsps0_message_handler,
			provider_config,
		}
	}

	fn handle_lsps_message(
		&self, msg: LSPSMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match msg {
			LSPSMessage::Invalid => {
				return Err(LightningError { err: format!("{} did not understand a message we previously sent, maybe they don't support a protocol we are trying to use?", sender_node_id), action: ErrorAction::IgnoreAndLog(Level::Error)});
			}
			LSPSMessage::LSPS0(msg) => {
				self.lsps0_message_handler.handle_message(msg, sender_node_id)?;
			}
		}
		Ok(())
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: LSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}
}

impl<ES: Deref> CustomMessageReader for LiquidityManager<ES>
where
	ES::Target: EntropySource,
{
	type CustomMessage = RawLSPSMessage;

	fn read<R: io::Read>(
		&self, message_type: u16, buffer: &mut R,
	) -> Result<Option<Self::CustomMessage>, lightning::ln::msgs::DecodeError> {
		match message_type {
			LSPS_MESSAGE_TYPE => Ok(Some(RawLSPSMessage::read(buffer)?)),
			_ => Ok(None),
		}
	}
}

impl<ES: Deref> CustomMessageHandler for LiquidityManager<ES>
where
	ES::Target: EntropySource,
{
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();

		match LSPSMessage::from_str_with_id_map(&msg.payload, &mut request_id_to_method_map) {
			Ok(msg) => self.handle_lsps_message(msg, sender_node_id),
			Err(_) => {
				self.enqueue_message(*sender_node_id, LSPSMessage::Invalid);
				Ok(())
			}
		}
	}

	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
		let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();
		self.pending_messages
			.lock()
			.unwrap()
			.drain(..)
			.map(|(public_key, lsps_message)| {
				if let Some((request_id, method_name)) = lsps_message.get_request_id_and_method() {
					request_id_to_method_map.insert(request_id, method_name);
				}
				(
					public_key,
					RawLSPSMessage { payload: serde_json::to_string(&lsps_message).unwrap() },
				)
			})
			.collect()
	}

	fn provided_node_features(&self) -> NodeFeatures {
		let mut features = NodeFeatures::empty();

		if self.provider_config.is_some() {
			features.set_optional_custom_bit(LSPS_FEATURE_BIT).unwrap();
		}

		features
	}

	fn provided_init_features(&self, _their_node_id: &PublicKey) -> InitFeatures {
		let mut features = InitFeatures::empty();

		if self.provider_config.is_some() {
			features.set_optional_custom_bit(LSPS_FEATURE_BIT).unwrap();
		}

		features
	}
}
