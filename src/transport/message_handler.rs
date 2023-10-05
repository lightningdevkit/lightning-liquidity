use crate::events::{Event, EventQueue};
use crate::transport::msgs::{LSPSMessage, RawLSPSMessage, LSPS_MESSAGE_TYPE_ID};
use crate::transport::protocol::LSPS0MessageHandler;

use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::chain::{self, BestBlock, Confirm, Filter, Listen};
use lightning::ln::channelmanager::{ChainParameters, ChannelManager};
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::CustomMessageReader;
use lightning::routing::router::Router;
use lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lightning::util::logger::{Level, Logger};
use lightning::util::ser::Readable;

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::secp256k1::PublicKey;
use bitcoin::BlockHash;

use std::collections::HashMap;
use std::convert::TryFrom;
use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};

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
pub struct LiquidityManager<
	ES: Deref,
	M: Deref,
	T: Deref,
	F: Deref,
	R: Deref,
	SP: Deref,
	L: Deref,
	NS: Deref,
	C: Deref,
> where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
	C::Target: Filter,
{
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	lsps0_message_handler: LSPS0MessageHandler<ES>,
	provider_config: Option<LiquidityProviderConfig>,
	channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
	chain_source: Option<C>,
	genesis_hash: BlockHash,
	best_block: RwLock<BestBlock>,
}

impl<
		ES: Deref,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		L: Deref,
		NS: Deref,
		C: Deref,
	> LiquidityManager<ES, M, T, F, R, SP, L, NS, C>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
	C::Target: Filter,
{
	/// Constructor for the LiquidityManager
	///
	/// Sets up the required protocol message handlers based on the given [`LiquidityProviderConfig`].
	pub fn new(
		entropy_source: ES, provider_config: Option<LiquidityProviderConfig>,
		channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>, chain_source: Option<C>,
		chain_params: ChainParameters,
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
			chain_source,
			genesis_hash: genesis_block(chain_params.network).header.block_hash(),
			best_block: RwLock::new(chain_params.best_block),
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
			_ => {
				return Err(LightningError {
					err: format!(
						"Received message without message handler configured. From node = {:?}",
						sender_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				});
			}
		}
		Ok(())
	}

	fn enqueue_message(&self, node_id: PublicKey, msg: LSPSMessage) {
		let mut pending_msgs = self.pending_messages.lock().unwrap();
		pending_msgs.push((node_id, msg));
	}
}

impl<
		ES: Deref,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		L: Deref,
		NS: Deref,
		C: Deref,
	> CustomMessageReader for LiquidityManager<ES, M, T, F, R, SP, L, NS, C>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
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

impl<
		ES: Deref,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		L: Deref,
		NS: Deref,
		C: Deref,
	> CustomMessageHandler for LiquidityManager<ES, M, T, F, R, SP, L, NS, C>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
	C::Target: Filter,
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

impl<
		ES: Deref,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		L: Deref,
		NS: Deref,
		C: Deref,
	> Listen for LiquidityManager<ES, M, T, F, R, SP, L, NS, C>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
	C::Target: Filter,
{
	fn filtered_block_connected(
		&self, header: &bitcoin::BlockHeader, txdata: &chain::transaction::TransactionData,
		height: u32,
	) {
		{
			let best_block = self.best_block.read().unwrap();
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
		{
			let mut best_block = self.best_block.write().unwrap();
			assert_eq!(best_block.block_hash(), header.block_hash(),
				"Blocks must be disconnected in chain-order - the disconnected header must be the last connected header");
			assert_eq!(best_block.height(), height,
				"Blocks must be disconnected in chain-order - the disconnected block must have the correct height");
			*best_block = BestBlock::new(header.prev_blockhash, new_height)
		}

		// TODO: Call block_disconnected on all sub-modules that require it, e.g., CRManager.
		// Internally this should call transaction_unconfirmed for all transactions that were
		// confirmed at a height <= the one we now disconnected.
	}
}

impl<
		ES: Deref,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		L: Deref,
		NS: Deref,
		C: Deref,
	> Confirm for LiquidityManager<ES, M, T, F, R, SP, L, NS, C>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	NS::Target: NodeSigner,
	C::Target: Filter,
{
	fn transactions_confirmed(
		&self, header: &bitcoin::BlockHeader, txdata: &chain::transaction::TransactionData,
		height: u32,
	) {
		// TODO: Call transactions_confirmed on all sub-modules that require it, e.g., CRManager.
	}

	fn transaction_unconfirmed(&self, txid: &bitcoin::Txid) {
		// TODO: Call transaction_unconfirmed on all sub-modules that require it, e.g., CRManager.
		// Internally this should call transaction_unconfirmed for all transactions that were
		// confirmed at a height <= the one we now unconfirmed.
	}

	fn best_block_updated(&self, header: &bitcoin::BlockHeader, height: u32) {
		// TODO: Call best_block_updated on all sub-modules that require it, e.g., CRManager.
	}

	fn get_relevant_txids(&self) -> Vec<(bitcoin::Txid, Option<bitcoin::BlockHash>)> {
		// TODO: Collect relevant txids from all sub-modules that, e.g., CRManager.
		Vec::new()
	}
}
