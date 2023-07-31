use crate::events::{Event, EventQueue, EventResult};
use crate::transport::msgs::{LSPSMessage, RawLSPSMessage, RequestId, LSPS_MESSAGE_TYPE};
use crate::transport::protocol::LSPS0MessageHandler;

use bitcoin::secp256k1::PublicKey;
use lightning::chain;
use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::ln::channelmanager::ChannelManager;
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::CustomMessageReader;
use lightning::routing::router::Router;
use lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lightning::util::logger::{Level, Logger};
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
	) -> Result<Option<Event>, LightningError>;
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
pub struct LiquidityManager<
	ES: Deref,
	M: Deref,
	T: Deref,
	F: Deref,
	R: Deref,
	SP: Deref,
	L: Deref,
	NS: Deref,
> where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
{
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	lsps0_message_handler: LSPS0MessageHandler<ES>,
	provider_config: Option<LiquidityProviderConfig>,
	channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
}

impl<ES: Deref, M: Deref, T: Deref, F: Deref, R: Deref, SP: Deref, L: Deref, NS: Deref>
	LiquidityManager<ES, M, T, F, R, SP, L, NS>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
{
	/// Constructor for the LiquidityManager
	///
	/// Sets up the required protocol message handlers based on the given [`LiquidityProviderConfig`].
	pub fn new(
		entropy_source: ES, provider_config: Option<LiquidityProviderConfig>,
		channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
	) -> Self
where {
		let pending_messages = Arc::new(Mutex::new(vec![]));

		let lsps0_message_handler =
			LSPS0MessageHandler::new(entropy_source, vec![], Arc::clone(&pending_messages));

		Self {
			pending_messages,
			pending_events: Arc::new(EventQueue::default()),
			request_id_to_method_map: Mutex::new(HashMap::new()),
			lsps0_message_handler,
			provider_config,
			channel_manager,
		}
	}

	/// Blocks until next event is ready and returns it
	///
	/// Typically you would spawn a thread or task that calls this in a loop
	pub fn wait_next_event(&self) -> Event {
		self.pending_events.wait_next_event()
	}

	/// Returns and clears all events without blocking
	///
	/// Typically you would spawn a thread or task that calls this in a loop
	pub fn get_and_clear_pending_events(&self) -> Vec<Event> {
		self.pending_events.get_and_clear_pending_events()
	}

	/// Returns the waiting event result and close
	pub fn wait_event_result(self, request_id: RequestId) -> EventResult {
		loop {
			let events = self.pending_events.get_and_clear_pending_events();
			for Event { id, result } in events {
				if id == request_id {
					return result;
				}
			}
		}
	}

	/// This allows the list the protocols of LSPS0
	pub fn list_protocols(
		&self, counterparty_node_id: PublicKey,
	) -> Result<RequestId, lightning::ln::msgs::LightningError> {
		Ok(self.lsps0_message_handler.list_protocols(counterparty_node_id))
	}

	fn handle_lsps_message(
		&self, msg: LSPSMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		match msg {
			LSPSMessage::Invalid => {
				return Err(LightningError { err: format!("{} did not understand a message we previously sent, maybe they don't support a protocol we are trying to use?", sender_node_id), action: ErrorAction::IgnoreAndLog(Level::Error)});
			}
			LSPSMessage::LSPS0(msg) => {
				if let Some(event) =
					self.lsps0_message_handler.handle_message(msg, sender_node_id)?
				{
					self.pending_events.enqueue(event);
				}
			}
		}
		Ok(())
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: LSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}
}

impl<ES: Deref, M: Deref, T: Deref, F: Deref, R: Deref, SP: Deref, L: Deref, NS: Deref>
	CustomMessageReader for LiquidityManager<ES, M, T, F, R, SP, L, NS>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
{
	type CustomMessage = RawLSPSMessage;

	fn read<RD: io::Read>(
		&self, message_type: u16, buffer: &mut RD,
	) -> Result<Option<Self::CustomMessage>, lightning::ln::msgs::DecodeError> {
		match message_type {
			LSPS_MESSAGE_TYPE => Ok(Some(RawLSPSMessage::read(buffer)?)),
			_ => Ok(None),
		}
	}
}

impl<ES: Deref, M: Deref, T: Deref, F: Deref, R: Deref, SP: Deref, L: Deref, NS: Deref>
	CustomMessageHandler for LiquidityManager<ES, M, T, F, R, SP, L, NS>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
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
