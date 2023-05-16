use crate::events::{Event, EventQueue};
use crate::jit_channel::channel_manager::JITChannelManager;
use crate::jit_channel::msgs::{OpeningFeeParams, RawOpeningFeeParams};
use crate::transport::msgs::{LSPSMessage, RawLSPSMessage, LSPS_MESSAGE_TYPE};
use crate::transport::protocol::LSPS0MessageHandler;

use bitcoin::secp256k1::PublicKey;
use lightning::chain;
use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::ln::channelmanager::ChannelManager;
use lightning::ln::channelmanager::InterceptId;
use lightning::ln::features::{InitFeatures, NodeFeatures};
use lightning::ln::msgs::{
	ChannelMessageHandler, ErrorAction, LightningError, OnionMessageHandler, RoutingMessageHandler,
};
use lightning::ln::peer_handler::{CustomMessageHandler, PeerManager, SocketDescriptor};
use lightning::ln::wire::CustomMessageReader;
use lightning::routing::router::Router;
use lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lightning::util::errors::APIError;
use lightning::util::logger::{Level, Logger};
use lightning::util::ser::Readable;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use super::msgs::RequestId;

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
	/// Optional configuration for jit channels
	/// should you want to support them
	pub jit_channels: Option<JITChannelsConfig>,
}

/// Configuration options for jit channels
/// A configuration used for the creation of Just In Time Channels.
pub struct JITChannelsConfig {
	/// Used to calculate the promise for channel parameters supplied to clients
	///
	/// Note: If this changes then old promises given out will be considered invalid
	pub promise_secret: [u8; 32],
}

/// The main interface into LSP functionality.
///
/// Should be used as a [`CustomMessageHandler`] for your
/// [`lightning::ln::peer_handler::PeerManager`]'s [`lightning::ln::peer_handler::MessageHandler`].
///
/// Should provide a reference to your [`lightning::ln::peer_handler::PeerManager`] by calling
/// [`LiquidityManager::set_peer_manager()`] post construction.  This allows the [`LiquidityManager`] to
/// wake the [`lightning::ln::peer_handler::PeerManager`] when there are pending messages to be sent.
///
/// Users need to continually poll [`LiquidityManager::get_and_clear_pending_events()`] in order to surface
/// [`Event`]'s that likely need to be handled.
///
/// Users must forward the [`lightning::events::Event::HTLCIntercepted`] event parameters to [`LiquidityManager::htlc_intercepted()`]
/// and the [`lightning::events::Event::ChannelReady`] event parameters to [`LiquidityManager::channel_ready()`].
pub struct LiquidityManager<
	ES: Deref + Clone,
	M: Deref,
	T: Deref,
	F: Deref,
	R: Deref,
	SP: Deref,
	Descriptor: SocketDescriptor,
	L: Deref,
	RM: Deref,
	CM: Deref,
	OM: Deref,
	NS: Deref,
> where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	RM::Target: RoutingMessageHandler,
	CM::Target: ChannelMessageHandler,
	OM::Target: OnionMessageHandler,
	NS::Target: NodeSigner,
{
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	request_id_to_method_map: Mutex<HashMap<String, String>>,
	lsps0_message_handler: LSPS0MessageHandler<ES>,
	lsps2_message_handler:
		Option<JITChannelManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, Arc<Self>, NS>>,
	provider_config: Option<LiquidityProviderConfig>,
	channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
}

impl<
		ES: Deref + Clone,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		Descriptor: SocketDescriptor,
		L: Deref,
		RM: Deref,
		CM: Deref,
		OM: Deref,
		NS: Deref,
	> LiquidityManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, NS>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	RM::Target: RoutingMessageHandler,
	CM::Target: ChannelMessageHandler,
	OM::Target: OnionMessageHandler,
	NS::Target: NodeSigner,
{
	/// Constructor for the [`LiquidityManager`]
	///
	/// Sets up the required protocol message handlers based on the given [`LiquidityProviderConfig`].
	pub fn new(
		entropy_source: ES, provider_config: Option<LiquidityProviderConfig>,
		channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
	) -> Self
where {
		let pending_messages = Arc::new(Mutex::new(vec![]));
		let pending_events = Arc::new(EventQueue::default());

		let lsps0_message_handler =
			LSPS0MessageHandler::new(entropy_source.clone(), vec![], Arc::clone(&pending_messages));

		let lsps2_message_handler = provider_config.as_ref().and_then(|config| {
			config.jit_channels.as_ref().map(|jit_channels_config| {
				JITChannelManager::new(
					entropy_source.clone(),
					jit_channels_config.promise_secret,
					Arc::clone(&pending_messages),
					Arc::clone(&pending_events),
					Arc::clone(&channel_manager),
				)
			})
		});

		Self {
			pending_messages,
			pending_events,
			request_id_to_method_map: Mutex::new(HashMap::new()),
			lsps0_message_handler,
			provider_config,
			channel_manager,
			lsps2_message_handler,
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

	/// Set a [`lightning::ln::peer_handler::PeerManager`] reference for the message handlers
	///
	/// This allows the message handlers to wake the [`lightning::ln::peer_handler::PeerManager`] by calling
	/// [`lightning::ln::peer_handler::PeerManager::process_events()`] after enqueing messages to be sent.
	///
	/// Without this the messages will be sent based on whatever polling interval
	/// your background processor uses.
	pub fn set_peer_manager(
		&self, peer_manager: Arc<PeerManager<Descriptor, CM, RM, OM, L, Arc<Self>, NS>>,
	) {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.set_peer_manager(peer_manager);
		}
	}

	/// Initiate the creation of an invoice that when paid will open a channel
	/// with enough inbound liquidity to be able to receive the payment.
	///
	/// `counterparty_node_id` is the node_id of the LSP you would like to use.
	///
	/// if `payment_size_msat` is [`Option::Some`] then the invoice will be for a fixed amount
	/// and MPP can be used to pay it.
	///
	/// if `payment_size_msat` is [`Option::None`] then the invoice can be for an arbitrary amount
	/// but MPP can no longer be used to pay it.
	///
	/// `token` is an optional String that will be provided to the LSP.
	/// it can be used by the LSP as an API key, coupon code, or some other way to identify a user.
	pub fn create_invoice(
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

	/// Used by LSP to provide fee parameters to a client requesting a JIT Channel.
	///
	/// Should be called in response to receiving a [`crate::JITChannelEvent::GetInfo`] event.
	pub fn opening_fee_params_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
		opening_fee_params_menu: Vec<RawOpeningFeeParams>, min_payment_size_msat: u64,
		max_payment_size_msat: u64,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.opening_fee_params_generated(
				counterparty_node_id,
				request_id,
				opening_fee_params_menu,
				min_payment_size_msat,
				max_payment_size_msat,
			)
		} else {
			Err(APIError::APIMisuseError {
				err: "JIT Channels were not configured when LSPManager was instantiated"
					.to_string(),
			})
		}
	}

	/// Used by client to confirm which channel parameters to use for the JIT Channel buy request.
	///
	/// Should be called in response to receiving a [`crate::JITChannelEvent::GetInfoResponse`] event.
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

	/// Used by LSP to provide client with the scid and cltv_expiry_delta to use in their invoice
	///
	/// Should be called in response to receiving a [`crate::JITChannelEvent::BuyRequest`] event.
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

	/// Forward [`lightning::events::Event::HTLCIntercepted`] event parameters into this function.
	///
	/// Will fail the intercepted HTLC if the scid matches a payment we are expecting
	/// but the payment amount is incorrect or the expiry has passed.
	///
	/// Will generate a [`crate::JITChannelEvent::OpenChannel`] event if the scid matches a payment we are expected
	/// and the payment amount is correct and the offer has not expired.
	///
	/// Will do nothing if the scid does not match any of the ones we gave out.
	pub fn htlc_intercepted(
		&self, scid: u64, intercept_id: InterceptId, inbound_amount_msat: u64,
		expected_outbound_amount_msat: u64,
	) -> Result<(), APIError> {
		if let Some(lsps2_message_handler) = &self.lsps2_message_handler {
			lsps2_message_handler.htlc_intercepted(
				scid,
				intercept_id,
				inbound_amount_msat,
				expected_outbound_amount_msat,
			)?;
		}

		Ok(())
	}

	/// Forward [`lightning::events::Event::ChannelReady`] event parameters into this function.
	///
	/// Will forward the intercepted HTLC if it matches a channel
	/// we need to forward a payment over otherwise it will be ignored.
	pub fn channel_ready(
		&self, user_channel_id: u128, channel_id: &[u8; 32], counterparty_node_id: &PublicKey,
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

impl<
		ES: Deref + Clone,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		Descriptor: SocketDescriptor,
		L: Deref,
		RM: Deref,
		CM: Deref,
		OM: Deref,
		NS: Deref,
	> CustomMessageReader for LiquidityManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, NS>
where
	ES::Target: EntropySource,
	L::Target: Logger,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	RM::Target: RoutingMessageHandler,
	CM::Target: ChannelMessageHandler,
	OM::Target: OnionMessageHandler,
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

impl<
		ES: Deref + Clone,
		M: Deref,
		T: Deref,
		F: Deref,
		R: Deref,
		SP: Deref,
		Descriptor: SocketDescriptor,
		L: Deref,
		RM: Deref,
		CM: Deref,
		OM: Deref,
		NS: Deref,
	> CustomMessageHandler for LiquidityManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, NS>
where
	ES::Target: EntropySource,
	M::Target: chain::Watch<<SP::Target as SignerProvider>::Signer>,
	T::Target: BroadcasterInterface,
	F::Target: FeeEstimator,
	R::Target: Router,
	SP::Target: SignerProvider,
	L::Target: Logger,
	RM::Target: RoutingMessageHandler,
	CM::Target: ChannelMessageHandler,
	OM::Target: OnionMessageHandler,
	NS::Target: NodeSigner,
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
