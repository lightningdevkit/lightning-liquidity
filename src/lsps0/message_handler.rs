#![allow(missing_docs)]

#[cfg(lsps1)]
use {
	crate::lsps1::channel_manager::CRManager,
	crate::lsps1::msgs::{ChannelInfo, OptionsSupported, Order, OrderId, OrderState, Payment},
};

use crate::events::{Event, EventQueue};
use crate::lsps0::msgs::RequestId;
use crate::lsps0::msgs::{LSPSMessage, RawLSPSMessage, LSPS_MESSAGE_TYPE_ID};
use crate::lsps0::protocol::LSPS0MessageHandler;
use crate::lsps2::channel_manager::JITChannelManager;
use crate::lsps2::msgs::{OpeningFeeParams, RawOpeningFeeParams};
use crate::prelude::{HashMap, String, ToString, Vec};
use crate::sync::{Arc, Mutex, RwLock};

use lightning::chain::{self, BestBlock, Confirm, Filter, Listen};
use lightning::ln::channelmanager::{AChannelManager, ChainParameters, InterceptId};
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::{APeerManager, CustomMessageHandler};
use lightning::ln::wire::CustomMessageReader;
use lightning::ln::ChannelId;
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;
use lightning::util::ser::Readable;

use bitcoin::blockdata::constants::genesis_block;
use bitcoin::secp256k1::PublicKey;
use bitcoin::BlockHash;

#[cfg(lsps1)]
use chrono::Utc;

use core::convert::TryFrom;
use core::ops::Deref;

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
pub struct LiquidityProviderConfig {
	/// LSPS1 Configuration
	#[cfg(lsps1)]
	pub lsps1_config: Option<CRChannelConfig>,
	/// Optional configuration for JIT channels
	/// should you want to support them.
	pub lsps2_config: Option<JITChannelsConfig>,
}

/// Configuration options for JIT channels.
pub struct JITChannelsConfig {
	/// Used to calculate the promise for channel parameters supplied to clients.
	///
	/// Note: If this changes then old promises given out will be considered invalid.
	pub promise_secret: [u8; 32],
	/// The minimum payment size you are willing to accept.
	pub min_payment_size_msat: u64,
	/// The maximum payment size you are willing to accept.
	pub max_payment_size_msat: u64,
}

#[cfg(lsps1)]
pub struct CRChannelConfig {
	pub token: Option<String>,
	pub max_fees: Option<u64>,
	pub options_supported: Option<OptionsSupported>,
	pub website: Option<String>,
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
/// Users must forward the [`Event::HTLCIntercepted`] event parameters to [`LiquidityManager::htlc_intercepted`]
/// and the [`Event::ChannelReady`] event parameters to [`LiquidityManager::channel_ready`].
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
	lsps1_message_handler: Option<CRManager<ES, CM, PM, C>>,
	lsps2_message_handler: Option<JITChannelManager<ES, CM, PM>>,
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
				JITChannelManager::new(
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
				CRManager::new(
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

	/// Set a [`PeerManager`] reference for the message handlers.
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

	#[cfg(lsps1)]
	pub fn request_for_info(
		&self, counterparty_node_id: PublicKey, channel_id: u128,
	) -> Result<(), APIError> {
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.request_for_info(counterparty_node_id, channel_id);
		}
		Ok(())
	}

	#[cfg(lsps1)]
	pub fn place_order(
		&self, channel_id: u128, counterparty_node_id: &PublicKey, order: Order,
	) -> Result<(), APIError> {
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.place_order(channel_id, counterparty_node_id, order)?;
		}
		Ok(())
	}

	#[cfg(lsps1)]
	pub fn send_invoice_for_order(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, payment: Payment,
		created_at: chrono::DateTime<Utc>, expires_at: chrono::DateTime<Utc>,
	) -> Result<(), APIError> {
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.send_invoice_for_order(
				request_id,
				counterparty_node_id,
				payment,
				created_at,
				expires_at,
			)?;
		}
		Ok(())
	}

	#[cfg(lsps1)]
	pub fn check_order_status(
		self, channel_id: u128, counterparty_node_id: &PublicKey, order_id: OrderId,
	) -> Result<(), APIError> {
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.check_order_status(counterparty_node_id, order_id, channel_id)?;
		}
		Ok(())
	}

	#[cfg(lsps1)]
	pub fn update_order_status(
		&self, request_id: RequestId, counterparty_node_id: PublicKey, order_id: OrderId,
		order_state: OrderState, channel: Option<ChannelInfo>,
	) -> Result<(), APIError> {
		if let Some(lsps1_message_handler) = &self.lsps1_message_handler {
			lsps1_message_handler.update_order_status(
				request_id,
				counterparty_node_id,
				order_id,
				order_state,
				channel,
			)?;
		}
		Ok(())
	}

	/// Initiate the creation of an invoice that when paid will open a channel
	/// with enough inbound liquidity to be able to receive the payment.
	///
	/// `counterparty_node_id` is the node_id of the LSP you would like to use.
	///
	/// If `payment_size_msat` is [`Option::Some`] then the invoice will be for a fixed amount
	/// and MPP can be used to pay it.
	///
	/// If `payment_size_msat` is [`Option::None`] then the invoice can be for an arbitrary amount
	/// but MPP can no longer be used to pay it.
	///
	/// `token` is an optional String that will be provided to the LSP.
	/// It can be used by the LSP as an API key, coupon code, or some other way to identify a user.
	pub fn lsps2_create_invoice(
		&self, counterparty_node_id: PublicKey, payment_size_msat: Option<u64>,
		token: Option<String>, user_channel_id: u128,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.create_invoice(
				counterparty_node_id,
				payment_size_msat,
				token,
				user_channel_id,
			);
			Ok(())
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Used by LSP to inform a client requesting a JIT Channel the token they used is invalid.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfo`] event.
	///
	/// [`LSPS2Event::GetInfo`]: crate::lsps2::LSPS2Event::GetInfo
	pub fn invalid_token_provided(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.invalid_token_provided(counterparty_node_id, request_id)
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Used by LSP to provide fee parameters to a client requesting a JIT Channel.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfo`] event.
	///
	/// [`LSPS2Event::GetInfo`]: crate::lsps2::LSPS2Event::GetInfo
	pub fn opening_fee_params_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
		opening_fee_params_menu: Vec<RawOpeningFeeParams>,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.opening_fee_params_generated(
				counterparty_node_id,
				request_id,
				opening_fee_params_menu,
			)
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Used by client to confirm which channel parameters to use for the JIT Channel buy request.
	/// The client agrees to paying an opening fee equal to
	/// `max(min_fee_msat, proportional*(payment_size_msat/1_000_000))`.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfoResponse`] event.
	///
	/// [`LSPS2Event::GetInfoResponse`]: crate::lsps2::LSPS2Event::GetInfoResponse
	pub fn opening_fee_params_selected(
		&self, counterparty_node_id: PublicKey, channel_id: u128,
		opening_fee_params: OpeningFeeParams,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.opening_fee_params_selected(
				counterparty_node_id,
				channel_id,
				opening_fee_params,
			)
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Used by LSP to provide client with the scid and cltv_expiry_delta to use in their invoice.
	///
	/// Should be called in response to receiving a [`LSPS2Event::BuyRequest`] event.
	///
	/// [`LSPS2Event::BuyRequest`]: crate::lsps2::LSPS2Event::BuyRequest
	pub fn invoice_parameters_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId, scid: u64,
		cltv_expiry_delta: u32, client_trusts_lsp: bool,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.invoice_parameters_generated(
				counterparty_node_id,
				request_id,
				scid,
				cltv_expiry_delta,
				client_trusts_lsp,
			)
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Forward [`Event::HTLCIntercepted`] event parameters into this function.
	///
	/// Will fail the intercepted HTLC if the scid matches a payment we are expecting
	/// but the payment amount is incorrect or the expiry has passed.
	///
	/// Will generate a [`LSPS2Event::OpenChannel`] event if the scid matches a payment we are expected
	/// and the payment amount is correct and the offer has not expired.
	///
	/// Will do nothing if the scid does not match any of the ones we gave out.
	///
	/// [`Event::HTLCIntercepted`]: lightning::events::Event::HTLCIntercepted
	/// [`LSPS2Event::OpenChannel`]: crate::lsps2::LSPS2Event::OpenChannel
	pub fn htlc_intercepted(
		&self, scid: u64, intercept_id: InterceptId, expected_outbound_amount_msat: u64,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.htlc_intercepted(
				scid,
				intercept_id,
				expected_outbound_amount_msat,
			)?;
		}

		Ok(())
	}

	/// Forward [`Event::ChannelReady`] event parameters into this function.
	///
	/// Will forward the intercepted HTLC if it matches a channel
	/// we need to forward a payment over otherwise it will be ignored.
	///
	/// [`Event::ChannelReady`]: lightning::events::Event::ChannelReady
	pub fn channel_ready(
		&self, user_channel_id: u128, channel_id: &ChannelId, counterparty_node_id: &PublicKey,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.channel_ready(
				user_channel_id,
				channel_id,
				counterparty_node_id,
			)?;
		}

		Ok(())
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

		// TODO: Call block_disconnected on all sub-modules that require it, e.g., CRManager.
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
