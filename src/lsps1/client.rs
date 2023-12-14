// This file is Copyright its original authors, visible in version contror
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Contains the main LSPS1 client object, [`LSPS1ClientHandler`].

use super::event::LSPS1ClientEvent;
use super::msgs::{
	CreateOrderRequest, CreateOrderResponse, GetInfoRequest, GetInfoResponse, GetOrderRequest,
	GetOrderResponse, LSPS1Message, LSPS1Request, LSPS1Response, OptionsSupported, OrderId,
	OrderParams,
};
use super::utils::is_valid;

use crate::events::EventQueue;
use crate::lsps0::message_handler::ProtocolMessageHandler;
use crate::lsps0::msgs::{LSPSMessage, RequestId};
use crate::prelude::{HashMap, String, ToString, Vec};
use crate::sync::{Arc, Mutex, RwLock};
use crate::utils;
use crate::{events::Event, lsps0::msgs::ResponseError};

use lightning::chain::Filter;
use lightning::ln::channelmanager::AChannelManager;
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::APeerManager;
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

const SUPPORTED_SPEC_VERSIONS: [u16; 1] = [1];

/// Client-side configuration options for LSPS1 channel requests.
#[derive(Clone, Debug)]
pub struct LSPS1ClientConfig {
	/// The maximally allowed channel fees.
	pub max_channel_fees_msat: Option<u64>,
}

struct ChannelStateError(String);

impl From<ChannelStateError> for LightningError {
	fn from(value: ChannelStateError) -> Self {
		LightningError { err: value.0, action: ErrorAction::IgnoreAndLog(Level::Info) }
	}
}

#[derive(PartialEq, Debug)]
enum InboundRequestState {
	InfoRequested,
	OptionsSupport { version: u16, options_supported: OptionsSupported },
	OrderRequested { version: u16, order: OrderParams },
	PendingPayment { order_id: OrderId },
	AwaitingConfirmation { id: u128, order_id: OrderId },
}

impl InboundRequestState {
	fn info_received(
		&self, versions: Vec<u16>, options: OptionsSupported,
	) -> Result<Self, ChannelStateError> {
		let max_shared_version = versions
			.iter()
			.filter(|version| SUPPORTED_SPEC_VERSIONS.contains(version))
			.max()
			.cloned()
			.ok_or(ChannelStateError(format!(
			"LSP does not support any of our specification versions.  ours = {:?}. theirs = {:?}",
			SUPPORTED_SPEC_VERSIONS, versions
		)))?;

		match self {
			InboundRequestState::InfoRequested => Ok(InboundRequestState::OptionsSupport {
				version: max_shared_version,
				options_supported: options,
			}),
			state => Err(ChannelStateError(format!(
				"Received unexpected get_versions response. Channel was in state: {:?}",
				state
			))),
		}
	}

	fn order_requested(&self, order: OrderParams) -> Result<Self, ChannelStateError> {
		match self {
			InboundRequestState::OptionsSupport { version, options_supported } => {
				if is_valid(&order, options_supported) {
					Ok(InboundRequestState::OrderRequested { version: *version, order })
				} else {
					return Err(ChannelStateError(format!(
						"The order created does not match options supported by LSP. Options Supported by LSP are {:?}. The order created was {:?}",
						options_supported, order
					)));
				}
			}
			state => Err(ChannelStateError(format!(
				"Received create order request for wrong channel. Channel was in state: {:?}",
				state
			))),
		}
	}

	fn order_received(
		&self, response_order: &OrderParams, order_id: OrderId,
	) -> Result<Self, ChannelStateError> {
		match self {
			InboundRequestState::OrderRequested { version, order } => {
				if response_order == order {
					Ok(InboundRequestState::PendingPayment { order_id })
				} else {
					Err(ChannelStateError(format!(
						"Received order is different from created order. The order created was : {:?}. Order Received from LSP is : {:?}",
						order, response_order
					)))
				}
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected create order response. Channel was in state: {:?}",
				state
			))),
		}
	}

	fn pay_for_channel(&self, channel_id: u128) -> Result<Self, ChannelStateError> {
		match self {
			InboundRequestState::PendingPayment { order_id } => {
				Ok(InboundRequestState::AwaitingConfirmation {
					id: channel_id,
					order_id: order_id.clone(),
				})
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected response. Channel was in state: {:?}",
				state
			))),
		}
	}
}

struct InboundCRChannel {
	id: u128,
	state: InboundRequestState,
}

impl InboundCRChannel {
	fn new(id: u128) -> Self {
		Self { id, state: InboundRequestState::InfoRequested }
	}

	fn info_received(
		&mut self, versions: Vec<u16>, options: OptionsSupported,
	) -> Result<u16, LightningError> {
		self.state = self.state.info_received(versions, options)?;

		match self.state {
			InboundRequestState::OptionsSupport { version, .. } => Ok(version),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	fn order_requested(&mut self, order: OrderParams) -> Result<u16, LightningError> {
		self.state = self.state.order_requested(order)?;

		match self.state {
			InboundRequestState::OrderRequested { version, .. } => Ok(version),
			_ => {
				return Err(LightningError {
					action: ErrorAction::IgnoreAndLog(Level::Error),
					err: "impossible state transition".to_string(),
				});
			}
		}
	}

	fn order_received(
		&mut self, order: &OrderParams, order_id: OrderId,
	) -> Result<(), LightningError> {
		self.state = self.state.order_received(order, order_id)?;
		Ok(())
	}

	fn pay_for_channel(&mut self, channel_id: u128) -> Result<(), LightningError> {
		self.state = self.state.pay_for_channel(channel_id)?;
		Ok(())
	}
}

#[derive(Default)]
struct PeerState {
	inbound_channels_by_id: HashMap<u128, InboundCRChannel>,
	request_to_cid: HashMap<RequestId, u128>,
	pending_requests: HashMap<RequestId, LSPS1Request>,
}

impl PeerState {
	fn insert_inbound_channel(&mut self, id: u128, channel: InboundCRChannel) {
		self.inbound_channels_by_id.insert(id, channel);
	}

	fn insert_request(&mut self, request_id: RequestId, channel_id: u128) {
		self.request_to_cid.insert(request_id, channel_id);
	}

	fn remove_inbound_channel(&mut self, id: u128) {
		self.inbound_channels_by_id.remove(&id);
	}
}

/// The main object allowing to send and receive LSPS1 messages.
pub struct LSPS1ClientHandler<ES: Deref, CM: Deref + Clone, PM: Deref + Clone, C: Deref>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	entropy_source: ES,
	channel_manager: CM,
	peer_manager: Mutex<Option<PM>>,
	chain_source: Option<C>,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	config: LSPS1ClientConfig,
}

impl<ES: Deref, CM: Deref + Clone, PM: Deref + Clone, C: Deref> LSPS1ClientHandler<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
	ES::Target: EntropySource,
{
	pub(crate) fn new(
		entropy_source: ES, pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
		pending_events: Arc<EventQueue>, channel_manager: CM, chain_source: Option<C>,
		config: LSPS1ClientConfig,
	) -> Self {
		Self {
			entropy_source,
			channel_manager,
			peer_manager: Mutex::new(None),
			chain_source,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			config,
		}
	}

	/// Set a [`PeerManager`] reference for the message handler.
	///
	/// This allows the message handler to wake the [`PeerManager`] by calling
	/// [`PeerManager::process_events`] after enqueing messages to be sent.
	///
	/// Without this the messages will be sent based on whatever polling interval
	/// your background processor uses.
	///
	/// [`PeerManager`]: lightning::ln::peer_handler::PeerManager
	/// [`PeerManager::process_events`]: lightning::ln::peer_handler::PeerManager::process_events
	pub fn set_peer_manager(&self, peer_manager: PM) {
		*self.peer_manager.lock().unwrap() = Some(peer_manager);
	}

	fn request_for_info(&self, counterparty_node_id: PublicKey, channel_id: u128) {
		let channel = InboundCRChannel::new(channel_id);

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock = outer_state_lock
			.entry(counterparty_node_id)
			.or_insert(Mutex::new(PeerState::default()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock.insert_inbound_channel(channel_id, channel);

		let request_id = self.generate_request_id();
		peer_state_lock.insert_request(request_id.clone(), channel_id);

		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages.push((
				counterparty_node_id,
				LSPS1Message::Request(request_id, LSPS1Request::GetInfo(GetInfoRequest {})).into(),
			));
		}

		if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
			peer_manager.as_ref().process_events();
		}
	}

	fn handle_get_info_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, result: GetInfoResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.write().unwrap();

		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let channel_id =
					peer_state_lock.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_info response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
					err: format!(
						"Received get_info response for an unknown channel: {:?}",
						channel_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})?;

				let version = match inbound_channel
					.info_received(result.supported_versions, result.options.clone())
				{
					Ok(version) => version,
					Err(e) => {
						peer_state_lock.remove_inbound_channel(channel_id);
						return Err(e);
					}
				};

				self.enqueue_event(Event::LSPS1Client(LSPS1ClientEvent::GetInfoResponse {
					id: channel_id,
					request_id,
					counterparty_node_id: *counterparty_node_id,
					version,
					website: result.website,
					options_supported: result.options,
				}))
			}
			None => {
				return Err(LightningError {
					err: format!(
						"Received get_info response from unknown peer: {:?}",
						counterparty_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})
			}
		}
		Ok(())
	}

	fn place_order(
		&self, channel_id: u128, counterparty_node_id: &PublicKey, order: OrderParams,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.write().unwrap();

		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(APIError::APIMisuseError {
					err: format!("Channel with id {} not found", channel_id),
				})?;

				let version = match inbound_channel.order_requested(order.clone()) {
					Ok(version) => version,
					Err(e) => {
						peer_state_lock.remove_inbound_channel(channel_id);
						return Err(APIError::APIMisuseError { err: e.err });
					}
				};

				let request_id = self.generate_request_id();
				peer_state_lock.insert_request(request_id.clone(), channel_id);

				{
					let mut pending_messages = self.pending_messages.lock().unwrap();
					pending_messages.push((
						*counterparty_node_id,
						LSPS1Message::Request(
							request_id,
							LSPS1Request::CreateOrder(CreateOrderRequest { order, version }),
						)
						.into(),
					));
				}
				if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
					peer_manager.as_ref().process_events();
				}
			}
			None => {
				return Err(APIError::APIMisuseError {
					err: format!("No existing state with counterparty {}", counterparty_node_id),
				})
			}
		}
		Ok(())
	}

	fn handle_create_order_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey,
		response: CreateOrderResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let channel_id =
					peer_state_lock.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received create_order response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
					err: format!(
						"Received create_order response for an unknown channel: {:?}",
						channel_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})?;

				if let Err(e) =
					inbound_channel.order_received(&response.order, response.order_id.clone())
				{
					peer_state_lock.remove_inbound_channel(channel_id);
					return Err(e);
				}

				let total_fees = response.payment.fee_total_sat + response.order.client_balance_sat;
				let max_channel_fees_msat = self.config.max_channel_fees_msat.unwrap_or(u64::MAX);

				if total_fees == response.payment.order_total_sat
					&& total_fees < max_channel_fees_msat
				{
					self.enqueue_event(Event::LSPS1Client(LSPS1ClientEvent::DisplayOrder {
						id: channel_id,
						counterparty_node_id: *counterparty_node_id,
						order: response.order,
						payment: response.payment,
						channel: response.channel,
					}));
				} else {
					peer_state_lock.remove_inbound_channel(channel_id);
					return Err(LightningError {
						err: format!("Fees are too high : {:?}", total_fees),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					});
				}
			}
			None => {
				return Err(LightningError {
					err: format!(
						"Received create_order response from unknown peer: {}",
						counterparty_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})
			}
		}

		Ok(())
	}

	fn handle_create_order_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, error: ResponseError,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let channel_id =
					peer_state_lock.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received create order error for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
					err: format!(
						"Received create order error for an unknown channel: {:?}",
						channel_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})?;
				Ok(())
			}
			None => {
				return Err(LightningError { err: format!("Received error response for a create order request from an unknown counterparty ({:?})",counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
			}
		}
	}

	fn check_order_status(
		&self, counterparty_node_id: &PublicKey, order_id: OrderId, channel_id: u128,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.write().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				if let Some(inbound_channel) =
					peer_state_lock.inbound_channels_by_id.get_mut(&channel_id)
				{
					if let Err(e) = inbound_channel.pay_for_channel(channel_id) {
						peer_state_lock.remove_inbound_channel(channel_id);
						return Err(APIError::APIMisuseError { err: e.err });
					}

					let request_id = self.generate_request_id();
					peer_state_lock.insert_request(request_id.clone(), channel_id);

					{
						let mut pending_messages = self.pending_messages.lock().unwrap();
						pending_messages.push((
							*counterparty_node_id,
							LSPS1Message::Request(
								request_id,
								LSPS1Request::GetOrder(GetOrderRequest {
									order_id: order_id.clone(),
								}),
							)
							.into(),
						));
					}
					if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
						peer_manager.as_ref().process_events();
					}
				} else {
					return Err(APIError::APIMisuseError {
						err: format!("Channel with id {} not found", channel_id),
					});
				}
			}
			None => {
				return Err(APIError::APIMisuseError {
					err: format!("No existing state with counterparty {}", counterparty_node_id),
				})
			}
		}

		Ok(())
	}

	fn handle_get_order_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: GetOrderResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let channel_id =
					peer_state_lock.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_versions response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
					err: format!(
						"Received get_versions response for an unknown channel: {:?}",
						channel_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})?;
			}
			None => {
				return Err(LightningError {
					err: format!(
						"Received get_order response from unknown peer: {}",
						counterparty_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})
			}
		}

		Ok(())
	}

	fn handle_get_order_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: ResponseError,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state_lock = inner_state_lock.lock().unwrap();

				let channel_id =
					peer_state_lock.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_order error for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let _inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
						err: format!(
							"Received get_order error for an unknown channel: {:?}",
							channel_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;
				Ok(())
			}
			None => {
				return Err(LightningError { err: format!("Received get_order response for a create order request from an unknown counterparty ({:?})",counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
			}
		}
	}

	fn enqueue_event(&self, event: Event) {
		self.pending_events.enqueue(event);
	}

	fn generate_channel_id(&self) -> u128 {
		let bytes = self.entropy_source.get_secure_random_bytes();
		let mut id_bytes: [u8; 16] = [0; 16];
		id_bytes.copy_from_slice(&bytes[0..16]);
		u128::from_be_bytes(id_bytes)
	}

	fn generate_request_id(&self) -> RequestId {
		let bytes = self.entropy_source.get_secure_random_bytes();
		RequestId(utils::hex_str(&bytes[0..16]))
	}

	fn generate_order_id(&self) -> OrderId {
		let bytes = self.entropy_source.get_secure_random_bytes();
		OrderId(utils::hex_str(&bytes[0..16]))
	}
}

impl<ES: Deref, CM: Deref + Clone, PM: Deref + Clone, C: Deref> ProtocolMessageHandler
	for LSPS1ClientHandler<ES, CM, PM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
	C::Target: Filter,
{
	type ProtocolMessage = LSPS1Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(1);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS1Message::Response(request_id, response) => match response {
				LSPS1Response::GetInfo(params) => {
					self.handle_get_info_response(request_id, counterparty_node_id, params)
				}
				LSPS1Response::CreateOrder(params) => {
					self.handle_create_order_response(request_id, counterparty_node_id, params)
				}
				LSPS1Response::CreateOrderError(params) => {
					self.handle_create_order_error(request_id, counterparty_node_id, params)
				}
				LSPS1Response::GetOrder(params) => {
					self.handle_get_order_response(request_id, counterparty_node_id, params)
				}
				LSPS1Response::GetOrderError(error) => {
					self.handle_get_order_error(request_id, counterparty_node_id, error)
				}
			},
			_ => {
				debug_assert!(
					false,
					"Client handler received LSPS1 request message. This should never happen."
				);
				Err(LightningError { err: format!("Client handler received LSPS1 request message from node {:?}. This should never happen.", counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)})
			}
		}
	}
}
