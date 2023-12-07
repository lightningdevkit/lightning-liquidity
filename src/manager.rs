use crate::events::{Event, EventQueue};
use crate::lsps0::message_handler::LSPS0MessageHandler;
use crate::lsps0::message_handler::ProtocolMessageHandler;
use crate::lsps0::msgs::{LSPSMessage, RawLSPSMessage, LSPS_MESSAGE_TYPE_ID};

#[cfg(lsps1)]
use {
	crate::lsps1::message_handler::{LSPS1Config, LSPS1MessageHandler},
	crate::lsps1::msgs::{ChannelInfo, OptionsSupported, Order, OrderId, OrderState, Payment},
};

use crate::lsps2::message_handler::{LSPS2Config, LSPS2MessageHandler};
use crate::prelude::{HashMap, String, Vec};
use crate::sync::{Arc, Mutex, RwLock};

use lightning::chain::{self, BestBlock, Confirm, Filter, Listen};
use lightning::ln::channelmanager::{AChannelManager, ChainParameters};
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::{APeerManager, CustomMessageHandler};
use lightning::ln::wire::CustomMessageReader;
use lightning::sign::EntropySource;
use lightning::util::logger::Level;
use lightning::util::ser::Readable;

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::secp256k1::PublicKey;
use bitcoin::BlockHash;

use core::ops::Deref;
const LSPS_FEATURE_BIT: usize = 729;

/// A configuration for [`LiquidityManager`].
///
/// Allows end-user to configure options when using the [`LiquidityManager`]
/// to provide liquidity services to clients.
pub struct LiquidityProviderConfig {
	/// LSPS1 Configuration
	#[cfg(lsps1)]
	pub lsps1_config: Option<LSPS1Config>,
	/// Optional configuration for JIT channels
	/// should you want to support them.
	pub lsps2_config: Option<LSPS2Config>,
}

/// The main interface into LSP functionality.
///
/// Should be used as a [`CustomMessageHandler`] for your
/// [`PeerManager`]'s [`MessageHandler`].
///
/// Should provide a reference to your [`PeerManager`] by calling
/// [`LiquidityManager::set_peer_manager`] post construction.  This allows the [`LiquidityManager`] to
/// wake the [`PeerManager`] when there are pending messages to be sent.
///
/// Users need to continually poll [`LiquidityManager::get_and_clear_pending_events`] in order to surface
/// [`Event`]'s that likely need to be handled.
///
/// If configured, users must forward the [`Event::HTLCIntercepted`] event parameters to [`LSPS2MessageHandler::htlc_intercepted`]
/// and the [`Event::ChannelReady`] event parameters to [`LSPS2MessageHandler::channel_ready`].
///
/// [`PeerManager`]: lightning::ln::peer_handler::PeerManager
/// [`MessageHandler`]: lightning::ln::peer_handler::MessageHandler
/// [`Event::HTLCIntercepted`]: lightning::events::Event::HTLCIntercepted
/// [`Event::ChannelReady`]: lightning::events::Event::ChannelReady
pub struct LiquidityManager<
	ES: Deref + Clone,
	CM: Deref + Clone,
	PM: Deref + Clone,
	C: Deref + Clone,
> where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	lsps0_message_handler: LSPS0MessageHandler<ES>,
	#[cfg(lsps1)]
	lsps1_message_handler: Option<LSPS1MessageHandler<ES, CM, PM, C>>,
	lsps2_message_handler: Option<LSPS2MessageHandler<ES, CM, PM>>,
	provider_config: Option<LiquidityProviderConfig>,
	channel_manager: CM,
	chain_source: Option<C>,
	genesis_hash: Option<BlockHash>,
	best_block: Option<RwLock<BestBlock>>,
}

impl<ES: Deref + Clone, CM: Deref + Clone, PM: Deref + Clone, C: Deref + Clone>
	LiquidityManager<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	/// Constructor for the [`LiquidityManager`].
	///
	/// Sets up the required protocol message handlers based on the given [`LiquidityProviderConfig`].
	pub fn new(
		entropy_source: ES, provider_config: Option<LiquidityProviderConfig>, channel_manager: CM,
		chain_source: Option<C>, chain_params: Option<ChainParameters>,
	) -> Self
where {
		let pending_messages = Arc::new(Mutex::new(vec![]));
		let pending_events = Arc::new(EventQueue::new());

		let lsps0_message_handler = LSPS0MessageHandler::new(
			entropy_source.clone().clone(),
			vec![],
			Arc::clone(&pending_messages),
		);

		let lsps2_message_handler = provider_config.as_ref().and_then(|config| {
			config.lsps2_config.as_ref().map(|config| {
				LSPS2MessageHandler::new(
					entropy_source.clone(),
					config,
					Arc::clone(&pending_messages),
					Arc::clone(&pending_events),
					channel_manager.clone(),
				)
			})
		});

		#[cfg(lsps1)]
		let lsps1_message_handler = provider_config.as_ref().and_then(|config| {
			config.lsps1_config.as_ref().map(|lsps1_config| {
				LSPS1MessageHandler::new(
					entropy_source.clone(),
					lsps1_config,
					Arc::clone(&pending_messages),
					Arc::clone(&pending_events),
					channel_manager.clone(),
					chain_source.clone(),
				)
			})
		});

		Self {
			pending_messages,
			pending_events,
			request_id_to_method_map: Mutex::new(HashMap::new()),
			lsps0_message_handler,
			#[cfg(lsps1)]
			lsps1_message_handler,
			lsps2_message_handler,
			provider_config,
			channel_manager,
			chain_source,
			genesis_hash: chain_params
				.map(|chain_params| genesis_block(chain_params.network).header.block_hash()),
			best_block: chain_params.map(|chain_params| RwLock::new(chain_params.best_block)),
		}
	}

	/// Returns a reference to the LSPS0 message handler.
	pub fn lsps0_message_handler(&self) -> &LSPS0MessageHandler<ES> {
		&self.lsps0_message_handler
	}

	/// Returns a reference to the LSPS1 message handler.
	#[cfg(lsps1)]
	pub fn lsps1_message_handler(&self) -> Option<&LSPS1MessageHandler<ES, CM, PM, C>> {
		self.lsps1_message_handler.as_ref()
	}

	/// Returns a reference to the LSPS2 message handler.
	pub fn lsps2_message_handler(&self) -> Option<&LSPS2MessageHandler<ES, CM, PM>> {
		self.lsps2_message_handler.as_ref()
	}

	/// Blocks the current thread until next event is ready and returns it.
	///
	/// Typically you would spawn a thread or task that calls this in a loop.
	#[cfg(feature = "std")]
	pub fn wait_next_event(&self) -> Event {
		self.pending_events.wait_next_event()
	}

	/// Returns `Some` if an event is ready.
	///
	/// Typically you would spawn a thread or task that calls this in a loop.
	pub fn next_event(&self) -> Option<Event> {
		self.pending_events.next_event()
	}

	/// Returns and clears all events without blocking.
	///
	/// Typically you would spawn a thread or task that calls this in a loop.
	pub fn get_and_clear_pending_events(&self) -> Vec<Event> {
		self.pending_events.get_and_clear_pending_events()
	}

	/// Set a [`PeerManager`] reference for all configured message handlers.
	///
	/// This allows the message handlers to wake the [`PeerManager`] by calling
	/// [`PeerManager::process_events`] after enqueing messages to be sent.
	///
	/// Without this the messages will be sent based on whatever polling interval
	/// your background processor uses.
	///
	/// [`PeerManager`]: lightning::ln::peer_handler::PeerManager
	/// [`PeerManager::process_events`]: lightning::ln::peer_handler::PeerManager::process_events
	pub fn set_peer_manager(&self, peer_manager: PM) {
		#[cfg(lsps1)]
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.set_peer_manager(peer_manager.clone());
		}
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.set_peer_manager(peer_manager);
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
			#[cfg(lsps1)]
			LSPSMessage::LSPS1(msg) => match &self.lsps1_message_handler {
				Some(lsps1_message_handler) => {
					lsps1_message_handler.handle_message(msg, sender_node_id)?;
				}
				None => {
					return Err(LightningError { err: format!("Received LSPS1 message without LSPS1 message handler configured. From node = {:?}", sender_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
				}
			},
			LSPSMessage::LSPS2(msg) => match &self.lsps2_message_handler {
				Some(lsps2_message_handler) => {
					lsps2_message_handler.handle_message(msg, sender_node_id)?;
				}
				None => {
					return Err(LightningError { err: format!("Received LSPS2 message without LSPS2 message handler configured. From node = {:?}", sender_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
				}
			},
		}
		Ok(())
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: LSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}
}

impl<ES: Deref + Clone + Clone, CM: Deref + Clone, PM: Deref + Clone, C: Deref + Clone>
	CustomMessageReader for LiquidityManager<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	type CustomMessage = RawLSPSMessage;

	fn read<RD: lightning::io::Read>(
		&self, message_type: u16, buffer: &mut RD,
	) -> Result<Option<Self::CustomMessage>, lightning::ln::msgs::DecodeError> {
		match message_type {
			LSPS_MESSAGE_TYPE_ID => Ok(Some(RawLSPSMessage::read(buffer)?)),
			_ => Ok(None),
		}
	}
}

impl<ES: Deref + Clone, CM: Deref + Clone, PM: Deref + Clone, C: Deref + Clone> CustomMessageHandler
	for LiquidityManager<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let message = {
			let mut request_id_to_method_map = self.request_id_to_method_map.lock().unwrap();
			LSPSMessage::from_str_with_id_map(&msg.payload, &mut request_id_to_method_map)
		};

		match message {
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

impl<ES: Deref + Clone, CM: Deref + Clone, PM: Deref + Clone, C: Deref + Clone> Listen
	for LiquidityManager<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	fn filtered_block_connected(
		&self, header: &bitcoin::BlockHeader, txdata: &chain::transaction::TransactionData,
		height: u32,
	) {
		if let Some(best_block) = &self.best_block {
			let best_block = best_block.read().unwrap();
			assert_eq!(best_block.block_hash(), header.prev_blockhash,
			"Blocks must be connected in chain-order - the connected header must build on the last connected header");
			assert_eq!(best_block.height(), height - 1,
			"Blocks must be connected in chain-order - the connected block height must be one greater than the previous height");
		}

		self.transactions_confirmed(header, txdata, height);
		self.best_block_updated(header, height);
	}

	fn block_disconnected(&self, header: &bitcoin::BlockHeader, height: u32) {
		let new_height = height - 1;
		if let Some(best_block) = &self.best_block {
			let mut best_block = best_block.write().unwrap();
			assert_eq!(best_block.block_hash(), header.block_hash(),
				"Blocks must be disconnected in chain-order - the disconnected header must be the last connected header");
			assert_eq!(best_block.height(), height,
				"Blocks must be disconnected in chain-order - the disconnected block must have the correct height");
			*best_block = BestBlock::new(header.prev_blockhash, new_height)
		}

		// TODO: Call block_disconnected on all sub-modules that require it, e.g., LSPS1MessageHandler.
		// Internally this should call transaction_unconfirmed for all transactions that were
		// confirmed at a height <= the one we now disconnected.
	}
}

impl<ES: Deref + Clone, CM: Deref + Clone, PM: Deref + Clone, C: Deref + Clone> Confirm
	for LiquidityManager<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	fn transactions_confirmed(
		&self, header: &bitcoin::BlockHeader, txdata: &chain::transaction::TransactionData,
		height: u32,
	) {
		// TODO: Call transactions_confirmed on all sub-modules that require it, e.g., LSPS1MessageHandler.
	}

	fn transaction_unconfirmed(&self, txid: &bitcoin::Txid) {
		// TODO: Call transaction_unconfirmed on all sub-modules that require it, e.g., LSPS1MessageHandler.
		// Internally this should call transaction_unconfirmed for all transactions that were
		// confirmed at a height <= the one we now unconfirmed.
	}

	fn best_block_updated(&self, header: &bitcoin::BlockHeader, height: u32) {
		// TODO: Call best_block_updated on all sub-modules that require it, e.g., LSPS1MessageHandler.
	}

	fn get_relevant_txids(&self) -> Vec<(bitcoin::Txid, Option<bitcoin::BlockHash>)> {
		// TODO: Collect relevant txids from all sub-modules that, e.g., LSPS1MessageHandler.
		Vec::new()
	}
}
