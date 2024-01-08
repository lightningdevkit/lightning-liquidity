// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Contains the main LSPS2 client object, [`LSPS2ClientHandler`].

use crate::events::{Event, EventQueue};
use crate::lsps0::msgs::{ProtocolMessageHandler, RequestId, ResponseError};
use crate::lsps2::event::LSPS2ClientEvent;
use crate::message_queue::MessageQueue;
use crate::prelude::{HashMap, String, ToString};
use crate::sync::{Arc, Mutex, RwLock};

use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::default::Default;
use core::ops::Deref;

use crate::lsps2::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, InterceptScid, LSPS2Message,
	LSPS2Request, LSPS2Response, OpeningFeeParams,
};

/// Client-side configuration options for JIT channels.
#[derive(Clone, Debug, Copy)]
pub struct LSPS2ClientConfig {
	/// Trust the LSP to create a valid channel funding transaction and have it confirmed on-chain.
	///
	/// TODO: If set to `false`, we'll only release the pre-image after we see an on-chain
	/// confirmation of the channel's funding transaction.
	///
	/// Defaults to `true`.
	pub client_trusts_lsp: bool,
}

impl Default for LSPS2ClientConfig {
	fn default() -> Self {
		Self { client_trusts_lsp: true }
	}
}

struct ChannelStateError(String);

impl From<ChannelStateError> for LightningError {
	fn from(value: ChannelStateError) -> Self {
		LightningError { err: value.0, action: ErrorAction::IgnoreAndLog(Level::Info) }
	}
}

struct InboundJITChannelConfig {
	pub user_id: u128,
	pub payment_size_msat: Option<u64>,
}

#[derive(PartialEq, Debug)]
enum InboundJITChannelState {
	MenuRequested,
	PendingMenuSelection,
	BuyRequested,
	PendingPayment { client_trusts_lsp: bool, intercept_scid: InterceptScid },
}

impl InboundJITChannelState {
	fn info_received(&self) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::MenuRequested => {
				Ok(InboundJITChannelState::PendingMenuSelection)
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected get_info response.  JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn opening_fee_params_selected(&self) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::PendingMenuSelection => {
				Ok(InboundJITChannelState::BuyRequested)
			}
			state => Err(ChannelStateError(format!(
				"Opening fee params selected when JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn invoice_params_received(
		&self, client_trusts_lsp: bool, intercept_scid: InterceptScid,
	) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::BuyRequested { .. } => {
				Ok(InboundJITChannelState::PendingPayment { client_trusts_lsp, intercept_scid })
			}
			state => Err(ChannelStateError(format!(
				"Invoice params received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}
}

struct InboundJITChannel {
	state: InboundJITChannelState,
	config: InboundJITChannelConfig,
}

impl InboundJITChannel {
	fn new(user_id: u128, payment_size_msat: Option<u64>) -> Self {
		Self {
			config: InboundJITChannelConfig { user_id, payment_size_msat },
			state: InboundJITChannelState::MenuRequested,
		}
	}

	fn info_received(&mut self) -> Result<(), LightningError> {
		self.state = self.state.info_received()?;
		Ok(())
	}

	fn opening_fee_params_selected(&mut self) -> Result<(), LightningError> {
		self.state = self.state.opening_fee_params_selected()?;

		match self.state {
			InboundJITChannelState::BuyRequested => Ok(()),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	fn invoice_params_received(
		&mut self, client_trusts_lsp: bool, intercept_scid: InterceptScid,
	) -> Result<(), LightningError> {
		self.state = self.state.invoice_params_received(client_trusts_lsp, intercept_scid)?;
		Ok(())
	}
}

struct PeerState {
	inbound_channels_by_id: HashMap<u128, InboundJITChannel>,
	request_to_cid: HashMap<RequestId, u128>,
}

impl PeerState {
	fn new() -> Self {
		let inbound_channels_by_id = HashMap::new();
		let request_to_cid = HashMap::new();
		Self { inbound_channels_by_id, request_to_cid }
	}

	fn insert_inbound_channel(&mut self, jit_channel_id: u128, channel: InboundJITChannel) {
		self.inbound_channels_by_id.insert(jit_channel_id, channel);
	}

	fn insert_request(&mut self, request_id: RequestId, jit_channel_id: u128) {
		self.request_to_cid.insert(request_id, jit_channel_id);
	}

	fn remove_inbound_channel(&mut self, jit_channel_id: u128) {
		self.inbound_channels_by_id.remove(&jit_channel_id);
	}
}

/// The main object allowing to send and receive LSPS2 messages.
pub struct LSPS2ClientHandler<ES: Deref>
where
	ES::Target: EntropySource,
{
	entropy_source: ES,
	pending_messages: Arc<MessageQueue>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	config: LSPS2ClientConfig,
}

impl<ES: Deref> LSPS2ClientHandler<ES>
where
	ES::Target: EntropySource,
{
	/// Constructs an `LSPS2ClientHandler`.
	pub(crate) fn new(
		entropy_source: ES, pending_messages: Arc<MessageQueue>, pending_events: Arc<EventQueue>,
		config: LSPS2ClientConfig,
	) -> Self {
		Self {
			entropy_source,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			config,
		}
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
	pub fn create_invoice(
		&self, counterparty_node_id: PublicKey, payment_size_msat: Option<u64>,
		token: Option<String>, user_channel_id: u128,
	) {
		let jit_channel_id = self.generate_jit_channel_id();
		let channel = InboundJITChannel::new(user_channel_id, payment_size_msat);

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock =
			outer_state_lock.entry(counterparty_node_id).or_insert(Mutex::new(PeerState::new()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock.insert_inbound_channel(jit_channel_id, channel);

		let request_id = crate::utils::generate_request_id(&self.entropy_source);
		peer_state_lock.insert_request(request_id.clone(), jit_channel_id);

		self.pending_messages.enqueue(
			&counterparty_node_id,
			LSPS2Message::Request(request_id, LSPS2Request::GetInfo(GetInfoRequest { token }))
				.into(),
		);
	}

	/// Used by client to confirm which channel parameters to use for the JIT Channel buy request.
	/// The client agrees to paying an opening fee equal to
	/// `max(min_fee_msat, proportional*(payment_size_msat/1_000_000))`.
	///
	/// Should be called in response to receiving a [`LSPS2ClientEvent::GetInfoResponse`] event.
	///
	/// [`LSPS2ClientEvent::GetInfoResponse`]: crate::lsps2::event::LSPS2ClientEvent::GetInfoResponse
	pub fn opening_fee_params_selected(
		&self, counterparty_node_id: PublicKey, jit_channel_id: u128,
		opening_fee_params: OpeningFeeParams,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();
				if let Some(jit_channel) =
					peer_state.inbound_channels_by_id.get_mut(&jit_channel_id)
				{
					match jit_channel.opening_fee_params_selected() {
						Ok(()) => (),
						Err(e) => {
							peer_state.remove_inbound_channel(jit_channel_id);
							return Err(APIError::APIMisuseError { err: e.err });
						}
					};

					let request_id = crate::utils::generate_request_id(&self.entropy_source);
					let payment_size_msat = jit_channel.config.payment_size_msat;
					peer_state.insert_request(request_id.clone(), jit_channel_id);

					self.pending_messages.enqueue(
						&counterparty_node_id,
						LSPS2Message::Request(
							request_id,
							LSPS2Request::Buy(BuyRequest { opening_fee_params, payment_size_msat }),
						)
						.into(),
					);
				} else {
					return Err(APIError::APIMisuseError {
						err: format!("Channel with id {} not found", jit_channel_id),
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

	fn generate_jit_channel_id(&self) -> u128 {
		let bytes = self.entropy_source.get_secure_random_bytes();
		let mut id_bytes: [u8; 16] = [0; 16];
		id_bytes.copy_from_slice(&bytes[0..16]);
		u128::from_be_bytes(id_bytes)
	}

	fn handle_get_info_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, result: GetInfoResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				let jit_channel_id =
					peer_state.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_info response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let jit_channel = peer_state
					.inbound_channels_by_id
					.get_mut(&jit_channel_id)
					.ok_or(LightningError {
						err: format!(
							"Received get_info response for an unknown channel: {:?}",
							jit_channel_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				if let Err(e) = jit_channel.info_received() {
					peer_state.remove_inbound_channel(jit_channel_id);
					return Err(e);
				}

				self.pending_events.enqueue(Event::LSPS2Client(
					LSPS2ClientEvent::GetInfoResponse {
						counterparty_node_id: *counterparty_node_id,
						opening_fee_params_menu: result.opening_fee_params_menu,
						min_payment_size_msat: result.min_payment_size_msat,
						max_payment_size_msat: result.max_payment_size_msat,
						jit_channel_id,
						user_channel_id: jit_channel.config.user_id,
					},
				));
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

	fn handle_get_info_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, _error: ResponseError,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				let jit_channel_id =
					peer_state.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_info error for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				peer_state.inbound_channels_by_id.remove(&jit_channel_id).ok_or(
					LightningError {
						err: format!(
							"Received get_info error for an unknown channel: {:?}",
							jit_channel_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					},
				)?;
				Ok(())
			}
			None => {
				return Err(LightningError { err: format!("Received error response for a get_info request from an unknown counterparty ({:?})",counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
			}
		}
	}

	fn handle_buy_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, result: BuyResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				let jit_channel_id =
					peer_state.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received buy response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let jit_channel = peer_state
					.inbound_channels_by_id
					.get_mut(&jit_channel_id)
					.ok_or(LightningError {
						err: format!(
							"Received buy response for an unknown channel: {:?}",
							jit_channel_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				// Reject the buy response if we disallow client_trusts_lsp and the LSP requires
				// it.
				if !self.config.client_trusts_lsp && result.client_trusts_lsp {
					peer_state.remove_inbound_channel(jit_channel_id);
					return Err(LightningError {
						err: format!(
							"Aborting JIT channel flow as the LSP requires 'client_trusts_lsp' mode, which we disallow"
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					});
				}

				if let Err(e) = jit_channel.invoice_params_received(
					result.client_trusts_lsp,
					result.intercept_scid.clone(),
				) {
					peer_state.remove_inbound_channel(jit_channel_id);
					return Err(e);
				}

				if let Ok(intercept_scid) = result.intercept_scid.to_scid() {
					self.pending_events.enqueue(Event::LSPS2Client(
						LSPS2ClientEvent::InvoiceGenerationReady {
							counterparty_node_id: *counterparty_node_id,
							intercept_scid,
							cltv_expiry_delta: result.lsp_cltv_expiry_delta,
							payment_size_msat: jit_channel.config.payment_size_msat,
							user_channel_id: jit_channel.config.user_id,
						},
					));
				} else {
					return Err(LightningError {
						err: format!(
							"Received buy response with an invalid intercept scid {:?}",
							result.intercept_scid
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					});
				}
			}
			None => {
				return Err(LightningError {
					err: format!(
						"Received buy response from unknown peer: {:?}",
						counterparty_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				});
			}
		}
		Ok(())
	}

	fn handle_buy_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, _error: ResponseError,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				let jit_channel_id =
					peer_state.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!("Received buy error for an unknown request: {:?}", request_id),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let _jit_channel = peer_state
					.inbound_channels_by_id
					.remove(&jit_channel_id)
					.ok_or(LightningError {
						err: format!(
							"Received buy error for an unknown channel: {:?}",
							jit_channel_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;
				Ok(())
			}
			None => {
				return Err(LightningError { err: format!("Received error response for a buy request from an unknown counterparty ({:?})",counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
			}
		}
	}
}

impl<ES: Deref> ProtocolMessageHandler for LSPS2ClientHandler<ES>
where
	ES::Target: EntropySource,
{
	type ProtocolMessage = LSPS2Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(2);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS2Message::Response(request_id, response) => match response {
				LSPS2Response::GetInfo(result) => {
					self.handle_get_info_response(request_id, counterparty_node_id, result)
				}
				LSPS2Response::GetInfoError(error) => {
					self.handle_get_info_error(request_id, counterparty_node_id, error)
				}
				LSPS2Response::Buy(result) => {
					self.handle_buy_response(request_id, counterparty_node_id, result)
				}
				LSPS2Response::BuyError(error) => {
					self.handle_buy_error(request_id, counterparty_node_id, error)
				}
			},
			_ => {
				debug_assert!(
					false,
					"Client handler received LSPS2 request message. This should never happen."
				);
				Err(LightningError { err: format!("Client handler received LSPS2 request message from node {:?}. This should never happen.", counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)})
			}
		}
	}
}

#[cfg(test)]
mod tests {}
