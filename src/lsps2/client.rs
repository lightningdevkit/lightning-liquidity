// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Contains the main LSPS2 client object, [`LSPS2ClientHandler`].

use crate::events::EventQueue;
use crate::lsps0::message_handler::ProtocolMessageHandler;
use crate::lsps0::msgs::{LSPSMessage, RequestId};
use crate::lsps2::event::LSPS2ClientEvent;
use crate::prelude::{HashMap, String, ToString, Vec};
use crate::sync::{Arc, Mutex, RwLock};
use crate::utils;
use crate::{events::Event, lsps0::msgs::ResponseError};

use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::APeerManager;
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

use crate::lsps2::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, GetVersionsRequest,
	GetVersionsResponse, JITChannelScid, LSPS2Message, LSPS2Request, LSPS2Response,
	OpeningFeeParams,
};

/// Client-side configuration options for JIT channels.
#[derive(Clone, Debug, Copy)]
pub struct LSPS2ClientConfig {}

const SUPPORTED_SPEC_VERSIONS: [u16; 1] = [1];

struct ChannelStateError(String);

impl From<ChannelStateError> for LightningError {
	fn from(value: ChannelStateError) -> Self {
		LightningError { err: value.0, action: ErrorAction::IgnoreAndLog(Level::Info) }
	}
}

struct InboundJITChannelConfig {
	pub user_id: u128,
	pub token: Option<String>,
	pub payment_size_msat: Option<u64>,
}

#[derive(PartialEq, Debug)]
enum InboundJITChannelState {
	VersionsRequested,
	MenuRequested { version: u16 },
	PendingMenuSelection { version: u16 },
	BuyRequested { version: u16 },
	PendingPayment { client_trusts_lsp: bool, short_channel_id: JITChannelScid },
}

impl InboundJITChannelState {
	fn versions_received(&self, versions: Vec<u16>) -> Result<Self, ChannelStateError> {
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
			InboundJITChannelState::VersionsRequested => {
				Ok(InboundJITChannelState::MenuRequested { version: max_shared_version })
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected get_versions response. JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn info_received(&self) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::MenuRequested { version } => {
				Ok(InboundJITChannelState::PendingMenuSelection { version: *version })
			}
			state => Err(ChannelStateError(format!(
				"Received unexpected get_info response.  JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn opening_fee_params_selected(&self) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::PendingMenuSelection { version } => {
				Ok(InboundJITChannelState::BuyRequested { version: *version })
			}
			state => Err(ChannelStateError(format!(
				"Opening fee params selected when JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn invoice_params_received(
		&self, client_trusts_lsp: bool, short_channel_id: JITChannelScid,
	) -> Result<Self, ChannelStateError> {
		match self {
			InboundJITChannelState::BuyRequested { .. } => {
				Ok(InboundJITChannelState::PendingPayment { client_trusts_lsp, short_channel_id })
			}
			state => Err(ChannelStateError(format!(
				"Invoice params received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}
}

struct InboundJITChannel {
	id: u128,
	state: InboundJITChannelState,
	config: InboundJITChannelConfig,
}

impl InboundJITChannel {
	fn new(id: u128, user_id: u128, payment_size_msat: Option<u64>, token: Option<String>) -> Self {
		Self {
			id,
			config: InboundJITChannelConfig { user_id, payment_size_msat, token },
			state: InboundJITChannelState::VersionsRequested,
		}
	}

	fn versions_received(&mut self, versions: Vec<u16>) -> Result<u16, LightningError> {
		self.state = self.state.versions_received(versions)?;

		match self.state {
			InboundJITChannelState::MenuRequested { version } => Ok(version),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	fn info_received(&mut self) -> Result<(), LightningError> {
		self.state = self.state.info_received()?;
		Ok(())
	}

	fn opening_fee_params_selected(&mut self) -> Result<u16, LightningError> {
		self.state = self.state.opening_fee_params_selected()?;

		match self.state {
			InboundJITChannelState::BuyRequested { version } => Ok(version),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	fn invoice_params_received(
		&mut self, client_trusts_lsp: bool, jit_channel_scid: JITChannelScid,
	) -> Result<(), LightningError> {
		self.state = self.state.invoice_params_received(client_trusts_lsp, jit_channel_scid)?;
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
pub struct LSPS2ClientHandler<ES: Deref, PM: Deref>
where
	ES::Target: EntropySource,
	PM::Target: APeerManager,
{
	entropy_source: ES,
	peer_manager: Mutex<Option<PM>>,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	_config: LSPS2ClientConfig,
}

impl<ES: Deref, PM: Deref> LSPS2ClientHandler<ES, PM>
where
	ES::Target: EntropySource,
	PM::Target: APeerManager,
{
	/// Constructs an `LSPS2ClientHandler`.
	pub(crate) fn new(
		entropy_source: ES, pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
		pending_events: Arc<EventQueue>, config: LSPS2ClientConfig,
	) -> Self {
		Self {
			entropy_source,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			peer_manager: Mutex::new(None),
			_config: config,
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
		let channel =
			InboundJITChannel::new(jit_channel_id, user_channel_id, payment_size_msat, token);

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock =
			outer_state_lock.entry(counterparty_node_id).or_insert(Mutex::new(PeerState::new()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock.insert_inbound_channel(jit_channel_id, channel);

		let request_id = self.generate_request_id();
		peer_state_lock.insert_request(request_id.clone(), jit_channel_id);

		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages.push((
				counterparty_node_id,
				LSPS2Message::Request(request_id, LSPS2Request::GetVersions(GetVersionsRequest {}))
					.into(),
			));
		}

		if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
			peer_manager.as_ref().process_events();
		}
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
					let version = match jit_channel.opening_fee_params_selected() {
						Ok(version) => version,
						Err(e) => {
							peer_state.remove_inbound_channel(jit_channel_id);
							return Err(APIError::APIMisuseError { err: e.err });
						}
					};

					let request_id = self.generate_request_id();
					let payment_size_msat = jit_channel.config.payment_size_msat;
					peer_state.insert_request(request_id.clone(), jit_channel_id);

					{
						let mut pending_messages = self.pending_messages.lock().unwrap();
						pending_messages.push((
							counterparty_node_id,
							LSPS2Message::Request(
								request_id,
								LSPS2Request::Buy(BuyRequest {
									version,
									opening_fee_params,
									payment_size_msat,
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

	fn generate_request_id(&self) -> RequestId {
		let bytes = self.entropy_source.get_secure_random_bytes();
		RequestId(utils::hex_str(&bytes[0..16]))
	}

	fn enqueue_event(&self, event: Event) {
		self.pending_events.enqueue(event);
	}

	fn handle_get_versions_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, result: GetVersionsResponse,
	) -> Result<(), LightningError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();
		match outer_state_lock.get(counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				let jit_channel_id =
					peer_state.request_to_cid.remove(&request_id).ok_or(LightningError {
						err: format!(
							"Received get_versions response for an unknown request: {:?}",
							request_id
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let jit_channel = peer_state
					.inbound_channels_by_id
					.get_mut(&jit_channel_id)
					.ok_or(LightningError {
						err: format!(
							"Received get_versions response for an unknown channel: {:?}",
							jit_channel_id,
						),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					})?;

				let token = jit_channel.config.token.clone();

				let version = match jit_channel.versions_received(result.versions) {
					Ok(version) => version,
					Err(e) => {
						peer_state.remove_inbound_channel(jit_channel_id);
						return Err(e);
					}
				};

				let request_id = self.generate_request_id();
				peer_state.insert_request(request_id.clone(), jit_channel_id);

				{
					let mut pending_messages = self.pending_messages.lock().unwrap();
					pending_messages.push((
						*counterparty_node_id,
						LSPS2Message::Request(
							request_id,
							LSPS2Request::GetInfo(GetInfoRequest { version, token }),
						)
						.into(),
					));
				}

				if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
					peer_manager.as_ref().process_events();
				}
			}
			None => {
				return Err(LightningError {
					err: format!(
						"Received get_versions response from unknown peer: {:?}",
						counterparty_node_id
					),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				})
			}
		}

		Ok(())
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

				self.enqueue_event(Event::LSPS2Client(LSPS2ClientEvent::GetInfoResponse {
					counterparty_node_id: *counterparty_node_id,
					opening_fee_params_menu: result.opening_fee_params_menu,
					min_payment_size_msat: result.min_payment_size_msat,
					max_payment_size_msat: result.max_payment_size_msat,
					jit_channel_id,
					user_channel_id: jit_channel.config.user_id,
				}));
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

				if let Err(e) = jit_channel.invoice_params_received(
					result.client_trusts_lsp,
					result.jit_channel_scid.clone(),
				) {
					peer_state.remove_inbound_channel(jit_channel_id);
					return Err(e);
				}

				if let Ok(scid) = result.jit_channel_scid.to_scid() {
					self.enqueue_event(Event::LSPS2Client(
						LSPS2ClientEvent::InvoiceGenerationReady {
							counterparty_node_id: *counterparty_node_id,
							scid,
							cltv_expiry_delta: result.lsp_cltv_expiry_delta,
							payment_size_msat: jit_channel.config.payment_size_msat,
							client_trusts_lsp: result.client_trusts_lsp,
							user_channel_id: jit_channel.config.user_id,
						},
					));
				} else {
					return Err(LightningError {
						err: format!(
							"Received buy response with an invalid scid {:?}",
							result.jit_channel_scid
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

impl<ES: Deref, PM: Deref> ProtocolMessageHandler for LSPS2ClientHandler<ES, PM>
where
	ES::Target: EntropySource,
	PM::Target: APeerManager,
{
	type ProtocolMessage = LSPS2Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(2);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS2Message::Response(request_id, response) => match response {
				LSPS2Response::GetVersions(result) => {
					self.handle_get_versions_response(request_id, counterparty_node_id, result)
				}
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