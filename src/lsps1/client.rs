// This file is Copyright its original authors, visible in version control
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
use crate::message_queue::MessageQueue;

use crate::events::EventQueue;
use crate::lsps0::msgs::{ProtocolMessageHandler, RequestId};
use crate::prelude::{HashMap, String, ToString, Vec};
use crate::sync::{Arc, Mutex, RwLock};
use crate::{events::Event, lsps0::msgs::ResponseError};

use lightning::chain::Filter;
use lightning::ln::channelmanager::AChannelManager;
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

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
	OptionsSupport { options_supported: OptionsSupported },
	OrderRequested { order: OrderParams },
	PendingPayment { order_id: OrderId },
	AwaitingConfirmation { id: u128, order_id: OrderId },
}

impl InboundRequestState {
	fn info_received(&self, options: OptionsSupported) -> Result<Self, ChannelStateError> {
		match self {
			InboundRequestState::InfoRequested => {
				Ok(InboundRequestState::OptionsSupport { options_supported: options })
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected get_info response. Channel was in state: {:?}",
				state
			))),
		}
	}

	fn order_requested(&self, order: OrderParams) -> Result<Self, ChannelStateError> {
		match self {
			InboundRequestState::OptionsSupport { options_supported } => {
				if is_valid(&order, options_supported) {
					Ok(InboundRequestState::OrderRequested { order })
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
			InboundRequestState::OrderRequested { order } => {
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

	fn info_received(&mut self, options: OptionsSupported) -> Result<(), LightningError> {
		self.state = self.state.info_received(options)?;

		match self.state {
			InboundRequestState::OptionsSupport { .. } => Ok(()),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	fn order_requested(&mut self, order: OrderParams) -> Result<(), LightningError> {
		self.state = self.state.order_requested(order)?;

		match self.state {
			InboundRequestState::OrderRequested { .. } => Ok(()),
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
pub struct LSPS1ClientHandler<ES: Deref, CM: Deref + Clone, C: Deref>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	C::Target: Filter,
{
	entropy_source: ES,
	channel_manager: CM,
	chain_source: Option<C>,
	pending_messages: Arc<MessageQueue>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	config: LSPS1ClientConfig,
}

impl<ES: Deref, CM: Deref + Clone, C: Deref> LSPS1ClientHandler<ES, CM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	C::Target: Filter,
	ES::Target: EntropySource,
{
	pub(crate) fn new(
		entropy_source: ES, pending_messages: Arc<MessageQueue>, pending_events: Arc<EventQueue>,
		channel_manager: CM, chain_source: Option<C>, config: LSPS1ClientConfig,
	) -> Self {
		Self {
			entropy_source,
			channel_manager,
			chain_source,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			config,
		}
	}

	fn request_for_info(&self, counterparty_node_id: PublicKey, channel_id: u128) {
		let channel = InboundCRChannel::new(channel_id);

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock = outer_state_lock
			.entry(counterparty_node_id)
			.or_insert(Mutex::new(PeerState::default()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock.insert_inbound_channel(channel_id, channel);

		let request_id = crate::utils::generate_request_id(&self.entropy_source);
		peer_state_lock.insert_request(request_id.clone(), channel_id);

		self.pending_messages.enqueue(
			&counterparty_node_id,
			LSPS1Message::Request(request_id, LSPS1Request::GetInfo(GetInfoRequest {})).into(),
		);
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

				match inbound_channel.info_received(result.options.clone()) {
					Ok(()) => (),
					Err(e) => {
						peer_state_lock.remove_inbound_channel(channel_id);
						return Err(e);
					}
				};

				self.pending_events.enqueue(Event::LSPS1Client(LSPS1ClientEvent::GetInfoResponse {
					id: channel_id,
					request_id,
					counterparty_node_id: *counterparty_node_id,
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

				match inbound_channel.order_requested(order.clone()) {
					Ok(()) => (),
					Err(e) => {
						peer_state_lock.remove_inbound_channel(channel_id);
						return Err(APIError::APIMisuseError { err: e.err });
					}
				};

				let request_id = crate::utils::generate_request_id(&self.entropy_source);
				peer_state_lock.insert_request(request_id.clone(), channel_id);

				self.pending_messages.enqueue(
					counterparty_node_id,
					LSPS1Message::Request(
						request_id,
						LSPS1Request::CreateOrder(CreateOrderRequest { order }),
					)
					.into(),
				);
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
					self.pending_events.enqueue(Event::LSPS1Client(
						LSPS1ClientEvent::DisplayOrder {
							id: channel_id,
							counterparty_node_id: *counterparty_node_id,
							order: response.order,
							payment: response.payment,
							channel: response.channel,
						},
					));
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

					let request_id = crate::utils::generate_request_id(&self.entropy_source);
					peer_state_lock.insert_request(request_id.clone(), channel_id);

					self.pending_messages.enqueue(
						counterparty_node_id,
						LSPS1Message::Request(
							request_id,
							LSPS1Request::GetOrder(GetOrderRequest { order_id: order_id.clone() }),
						)
						.into(),
					);
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
							"Received get_order response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let inbound_channel = peer_state_lock
					.inbound_channels_by_id
					.get_mut(&channel_id)
					.ok_or(LightningError {
					err: format!(
						"Received get_order response for an unknown channel: {:?}",
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
}

impl<ES: Deref, CM: Deref + Clone, C: Deref> ProtocolMessageHandler
	for LSPS1ClientHandler<ES, CM, C>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
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
