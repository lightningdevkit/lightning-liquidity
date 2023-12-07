// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Contains the main LSPS2 object, `LSPS2MessageHandler`.

use crate::events::EventQueue;
use crate::lsps0::message_handler::ProtocolMessageHandler;
use crate::lsps0::msgs::{LSPSMessage, RequestId};
use crate::lsps2::event::LSPS2Event;
use crate::lsps2::utils::{compute_opening_fee, is_valid_opening_fee_params};
use crate::prelude::{HashMap, String, ToString, Vec};
use crate::sync::{Arc, Mutex, RwLock};
use crate::utils;
use crate::{events::Event, lsps0::msgs::ResponseError};

use lightning::ln::channelmanager::{AChannelManager, InterceptId};
use lightning::ln::msgs::{ErrorAction, LightningError};
use lightning::ln::peer_handler::APeerManager;
use lightning::ln::ChannelId;
use lightning::sign::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Level;

use bitcoin::secp256k1::PublicKey;

use core::convert::TryInto;
use core::ops::Deref;

use crate::lsps2::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, GetVersionsRequest,
	GetVersionsResponse, JITChannelScid, LSPS2Message, LSPS2Request, LSPS2Response,
	OpeningFeeParams, RawOpeningFeeParams, LSPS2_BUY_REQUEST_INVALID_OPENING_FEE_PARAMS_ERROR_CODE,
	LSPS2_BUY_REQUEST_INVALID_VERSION_ERROR_CODE,
	LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_LARGE_ERROR_CODE,
	LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_SMALL_ERROR_CODE,
	LSPS2_GET_INFO_REQUEST_INVALID_VERSION_ERROR_CODE,
	LSPS2_GET_INFO_REQUEST_UNRECOGNIZED_OR_STALE_TOKEN_ERROR_CODE,
};

/// Configuration options for JIT channels.
pub struct LSPS2Config {
	/// Used to calculate the promise for channel parameters supplied to clients.
	///
	/// Note: If this changes then old promises given out will be considered invalid.
	pub promise_secret: [u8; 32],
	/// The minimum payment size you are willing to accept.
	pub min_payment_size_msat: u64,
	/// The maximum payment size you are willing to accept.
	pub max_payment_size_msat: u64,
}

const SUPPORTED_SPEC_VERSIONS: [u16; 1] = [1];

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct InterceptedHTLC {
	intercept_id: InterceptId,
	expected_outbound_amount_msat: u64,
}

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

#[derive(PartialEq, Debug)]
enum OutboundJITChannelState {
	AwaitingPayment {
		min_fee_msat: u64,
		proportional_fee: u32,
		htlcs: Vec<InterceptedHTLC>,
		payment_size_msat: Option<u64>,
	},
	PendingChannelOpen {
		htlcs: Vec<InterceptedHTLC>,
		opening_fee_msat: u64,
		amt_to_forward_msat: u64,
	},
	ChannelReady {
		htlcs: Vec<InterceptedHTLC>,
		amt_to_forward_msat: u64,
	},
}

impl OutboundJITChannelState {
	fn new(payment_size_msat: Option<u64>, opening_fee_params: OpeningFeeParams) -> Self {
		OutboundJITChannelState::AwaitingPayment {
			min_fee_msat: opening_fee_params.min_fee_msat,
			proportional_fee: opening_fee_params.proportional,
			htlcs: vec![],
			payment_size_msat,
		}
	}

	fn htlc_intercepted(&self, htlc: InterceptedHTLC) -> Result<Self, ChannelStateError> {
		match self {
			OutboundJITChannelState::AwaitingPayment {
				htlcs,
				payment_size_msat,
				min_fee_msat,
				proportional_fee,
			} => {
				let mut htlcs = htlcs.clone();
				htlcs.push(htlc);

				let total_expected_outbound_amount_msat =
					htlcs.iter().map(|htlc| htlc.expected_outbound_amount_msat).sum();

				let expected_payment_size_msat =
					payment_size_msat.unwrap_or(total_expected_outbound_amount_msat);

				let opening_fee_msat = compute_opening_fee(
					expected_payment_size_msat,
					*min_fee_msat,
					(*proportional_fee).into(),
				).ok_or(ChannelStateError(
					format!("Could not compute valid opening fee with min_fee_msat = {}, proportional = {}, and total_expected_outbound_amount_msat = {}", 
						min_fee_msat,
						proportional_fee,
						total_expected_outbound_amount_msat
					)
				))?;

				let amt_to_forward_msat =
					expected_payment_size_msat.saturating_sub(opening_fee_msat);

				if total_expected_outbound_amount_msat >= expected_payment_size_msat
					&& amt_to_forward_msat > 0
				{
					Ok(OutboundJITChannelState::PendingChannelOpen {
						htlcs,
						opening_fee_msat,
						amt_to_forward_msat,
					})
				} else {
					// payment size being specified means MPP is supported
					if payment_size_msat.is_some() {
						Ok(OutboundJITChannelState::AwaitingPayment {
							min_fee_msat: *min_fee_msat,
							proportional_fee: *proportional_fee,
							htlcs,
							payment_size_msat: *payment_size_msat,
						})
					} else {
						Err(ChannelStateError("HTLC is too small to pay opening fee".to_string()))
					}
				}
			}
			state => Err(ChannelStateError(format!(
				"Invoice params received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	fn channel_ready(&self) -> Result<Self, ChannelStateError> {
		match self {
			OutboundJITChannelState::PendingChannelOpen { htlcs, amt_to_forward_msat, .. } => {
				Ok(OutboundJITChannelState::ChannelReady {
					htlcs: htlcs.clone(),
					amt_to_forward_msat: *amt_to_forward_msat,
				})
			}
			state => Err(ChannelStateError(format!(
				"Channel ready received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}
}

struct OutboundJITChannel {
	state: OutboundJITChannelState,
	scid: u64,
	cltv_expiry_delta: u32,
	client_trusts_lsp: bool,
}

impl OutboundJITChannel {
	fn new(
		scid: u64, cltv_expiry_delta: u32, client_trusts_lsp: bool, payment_size_msat: Option<u64>,
		opening_fee_params: OpeningFeeParams,
	) -> Self {
		Self {
			scid,
			cltv_expiry_delta,
			client_trusts_lsp,
			state: OutboundJITChannelState::new(payment_size_msat, opening_fee_params),
		}
	}

	fn htlc_intercepted(
		&mut self, htlc: InterceptedHTLC,
	) -> Result<Option<(u64, u64)>, LightningError> {
		self.state = self.state.htlc_intercepted(htlc)?;

		match &self.state {
			OutboundJITChannelState::AwaitingPayment { htlcs, payment_size_msat, .. } => {
				// TODO: log that we received an htlc but are still awaiting payment
				Ok(None)
			}
			OutboundJITChannelState::PendingChannelOpen {
				opening_fee_msat,
				amt_to_forward_msat,
				..
			} => Ok(Some((*opening_fee_msat, *amt_to_forward_msat))),
			impossible_state => Err(LightningError {
				err: format!(
					"Impossible state transition during htlc_intercepted to {:?}",
					impossible_state
				),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			}),
		}
	}

	fn channel_ready(&mut self) -> Result<(Vec<InterceptedHTLC>, u64), LightningError> {
		self.state = self.state.channel_ready()?;

		match &self.state {
			OutboundJITChannelState::ChannelReady { htlcs, amt_to_forward_msat } => {
				Ok((htlcs.clone(), *amt_to_forward_msat))
			}
			impossible_state => Err(LightningError {
				err: format!(
					"Impossible state transition during channel_ready to {:?}",
					impossible_state
				),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			}),
		}
	}
}

struct PeerState {
	inbound_channels_by_id: HashMap<u128, InboundJITChannel>,
	outbound_channels_by_scid: HashMap<u64, OutboundJITChannel>,
	request_to_cid: HashMap<RequestId, u128>,
	pending_requests: HashMap<RequestId, LSPS2Request>,
}

impl PeerState {
	fn new() -> Self {
		let inbound_channels_by_id = HashMap::new();
		let outbound_channels_by_scid = HashMap::new();
		let request_to_cid = HashMap::new();
		let pending_requests = HashMap::new();
		Self { inbound_channels_by_id, outbound_channels_by_scid, request_to_cid, pending_requests }
	}

	fn insert_inbound_channel(&mut self, jit_channel_id: u128, channel: InboundJITChannel) {
		self.inbound_channels_by_id.insert(jit_channel_id, channel);
	}

	fn insert_outbound_channel(&mut self, scid: u64, channel: OutboundJITChannel) {
		self.outbound_channels_by_scid.insert(scid, channel);
	}

	fn insert_request(&mut self, request_id: RequestId, jit_channel_id: u128) {
		self.request_to_cid.insert(request_id, jit_channel_id);
	}

	fn remove_inbound_channel(&mut self, jit_channel_id: u128) {
		self.inbound_channels_by_id.remove(&jit_channel_id);
	}

	fn remove_outbound_channel(&mut self, scid: u64) {
		self.outbound_channels_by_scid.remove(&scid);
	}
}

/// The main object allowing to send and receive LSPS2 messages.
pub struct LSPS2MessageHandler<ES: Deref, CM: Deref + Clone, PM: Deref>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
{
	entropy_source: ES,
	peer_manager: Mutex<Option<PM>>,
	channel_manager: CM,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	peer_by_scid: RwLock<HashMap<u64, PublicKey>>,
	promise_secret: [u8; 32],
	min_payment_size_msat: u64,
	max_payment_size_msat: u64,
}

impl<ES: Deref, CM: Deref + Clone, PM: Deref> LSPS2MessageHandler<ES, CM, PM>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
{
	/// Constructs a `LSPS2MessageHandler`.
	pub(crate) fn new(
		entropy_source: ES, config: &LSPS2Config,
		pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
		pending_events: Arc<EventQueue>, channel_manager: CM,
	) -> Self {
		Self {
			entropy_source,
			promise_secret: config.promise_secret,
			min_payment_size_msat: config.min_payment_size_msat,
			max_payment_size_msat: config.max_payment_size_msat,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			peer_by_scid: RwLock::new(HashMap::new()),
			peer_manager: Mutex::new(None),
			channel_manager,
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

	/// Used by LSP to inform a client requesting a JIT Channel the token they used is invalid.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfo`] event.
	///
	/// [`LSPS2Event::GetInfo`]: crate::lsps2::event::LSPS2Event::GetInfo
	pub fn invalid_token_provided(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();

		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				match peer_state.pending_requests.remove(&request_id) {
					Some(LSPS2Request::GetInfo(_)) => {
						let response = LSPS2Response::GetInfoError(ResponseError {
							code: LSPS2_GET_INFO_REQUEST_UNRECOGNIZED_OR_STALE_TOKEN_ERROR_CODE,
							message: "an unrecognized or stale token was provided".to_string(),
							data: None,
						});
						self.enqueue_response(counterparty_node_id, request_id, response);
						Ok(())
					}
					_ => Err(APIError::APIMisuseError {
						err: format!(
							"No pending get_info request for request_id: {:?}",
							request_id
						),
					}),
				}
			}
			None => Err(APIError::APIMisuseError {
				err: format!("No state for the counterparty exists: {:?}", counterparty_node_id),
			}),
		}
	}

	/// Used by LSP to provide fee parameters to a client requesting a JIT Channel.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfo`] event.
	///
	/// [`LSPS2Event::GetInfo`]: crate::lsps2::event::LSPS2Event::GetInfo
	pub fn opening_fee_params_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
		opening_fee_params_menu: Vec<RawOpeningFeeParams>,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();

		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				match peer_state.pending_requests.remove(&request_id) {
					Some(LSPS2Request::GetInfo(_)) => {
						let response = LSPS2Response::GetInfo(GetInfoResponse {
							opening_fee_params_menu: opening_fee_params_menu
								.into_iter()
								.map(|param| param.into_opening_fee_params(&self.promise_secret))
								.collect(),
							min_payment_size_msat: self.min_payment_size_msat,
							max_payment_size_msat: self.max_payment_size_msat,
						});
						self.enqueue_response(counterparty_node_id, request_id, response);
						Ok(())
					}
					_ => Err(APIError::APIMisuseError {
						err: format!(
							"No pending get_info request for request_id: {:?}",
							request_id
						),
					}),
				}
			}
			None => Err(APIError::APIMisuseError {
				err: format!("No state for the counterparty exists: {:?}", counterparty_node_id),
			}),
		}
	}

	/// Used by client to confirm which channel parameters to use for the JIT Channel buy request.
	/// The client agrees to paying an opening fee equal to
	/// `max(min_fee_msat, proportional*(payment_size_msat/1_000_000))`.
	///
	/// Should be called in response to receiving a [`LSPS2Event::GetInfoResponse`] event.
	///
	/// [`LSPS2Event::GetInfoResponse`]: crate::lsps2::event::LSPS2Event::GetInfoResponse
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

	/// Used by LSP to provide client with the scid and cltv_expiry_delta to use in their invoice.
	///
	/// Should be called in response to receiving a [`LSPS2Event::BuyRequest`] event.
	///
	/// [`LSPS2Event::BuyRequest`]: crate::lsps2::event::LSPS2Event::BuyRequest
	pub fn invoice_parameters_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId, scid: u64,
		cltv_expiry_delta: u32, client_trusts_lsp: bool,
	) -> Result<(), APIError> {
		let outer_state_lock = self.per_peer_state.read().unwrap();

		match outer_state_lock.get(&counterparty_node_id) {
			Some(inner_state_lock) => {
				let mut peer_state = inner_state_lock.lock().unwrap();

				match peer_state.pending_requests.remove(&request_id) {
					Some(LSPS2Request::Buy(buy_request)) => {
						{
							let mut peer_by_scid = self.peer_by_scid.write().unwrap();
							peer_by_scid.insert(scid, counterparty_node_id);
						}

						let outbound_jit_channel = OutboundJITChannel::new(
							scid,
							cltv_expiry_delta,
							client_trusts_lsp,
							buy_request.payment_size_msat,
							buy_request.opening_fee_params,
						);

						peer_state.insert_outbound_channel(scid, outbound_jit_channel);

						self.enqueue_response(
							counterparty_node_id,
							request_id,
							LSPS2Response::Buy(BuyResponse {
								jit_channel_scid: scid.into(),
								lsp_cltv_expiry_delta: cltv_expiry_delta,
								client_trusts_lsp,
							}),
						);

						Ok(())
					}
					_ => Err(APIError::APIMisuseError {
						err: format!("No pending buy request for request_id: {:?}", request_id),
					}),
				}
			}
			None => Err(APIError::APIMisuseError {
				err: format!("No state for the counterparty exists: {:?}", counterparty_node_id),
			}),
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
	/// [`LSPS2Event::OpenChannel`]: crate::lsps2::event::LSPS2Event::OpenChannel
	pub fn htlc_intercepted(
		&self, scid: u64, intercept_id: InterceptId, expected_outbound_amount_msat: u64,
	) -> Result<(), APIError> {
		let peer_by_scid = self.peer_by_scid.read().unwrap();
		if let Some(counterparty_node_id) = peer_by_scid.get(&scid) {
			let outer_state_lock = self.per_peer_state.read().unwrap();
			match outer_state_lock.get(counterparty_node_id) {
				Some(inner_state_lock) => {
					let mut peer_state = inner_state_lock.lock().unwrap();
					if let Some(jit_channel) = peer_state.outbound_channels_by_scid.get_mut(&scid) {
						let htlc = InterceptedHTLC { intercept_id, expected_outbound_amount_msat };
						match jit_channel.htlc_intercepted(htlc) {
							Ok(Some((opening_fee_msat, amt_to_forward_msat))) => {
								self.enqueue_event(Event::LSPS2(LSPS2Event::OpenChannel {
									their_network_key: counterparty_node_id.clone(),
									amt_to_forward_msat,
									opening_fee_msat,
									user_channel_id: scid as u128,
								}));
							}
							Ok(None) => {}
							Err(e) => {
								self.channel_manager
									.get_cm()
									.fail_intercepted_htlc(intercept_id)?;
								peer_state.outbound_channels_by_scid.remove(&scid);
								// TODO: cleanup peer_by_scid
								return Err(APIError::APIMisuseError { err: e.err });
							}
						}
					}
				}
				None => {
					return Err(APIError::APIMisuseError {
						err: format!("No counterparty found for scid: {}", scid),
					});
				}
			}
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
		if let Ok(scid) = user_channel_id.try_into() {
			let outer_state_lock = self.per_peer_state.read().unwrap();
			match outer_state_lock.get(counterparty_node_id) {
				Some(inner_state_lock) => {
					let mut peer_state = inner_state_lock.lock().unwrap();
					if let Some(jit_channel) = peer_state.outbound_channels_by_scid.get_mut(&scid) {
						match jit_channel.channel_ready() {
							Ok((htlcs, total_amt_to_forward_msat)) => {
								let amounts_to_forward_msat = calculate_amount_to_forward_per_htlc(
									&htlcs,
									total_amt_to_forward_msat,
								);

								for (intercept_id, amount_to_forward_msat) in
									amounts_to_forward_msat
								{
									self.channel_manager.get_cm().forward_intercepted_htlc(
										intercept_id,
										channel_id,
										*counterparty_node_id,
										amount_to_forward_msat,
									)?;
								}
							}
							Err(e) => {
								return Err(APIError::APIMisuseError {
									err: format!(
										"Failed to transition to channel ready: {}",
										e.err
									),
								})
							}
						}
					} else {
						return Err(APIError::APIMisuseError {
							err: format!(
								"Could not find a channel with user_channel_id {}",
								user_channel_id
							),
						});
					}
				}
				None => {
					return Err(APIError::APIMisuseError {
						err: format!("No counterparty state for: {}", counterparty_node_id),
					});
				}
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

	fn enqueue_response(
		&self, counterparty_node_id: PublicKey, request_id: RequestId, response: LSPS2Response,
	) {
		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages
				.push((counterparty_node_id, LSPS2Message::Response(request_id, response).into()));
		}

		if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
			peer_manager.as_ref().process_events();
		}
	}

	fn enqueue_event(&self, event: Event) {
		self.pending_events.enqueue(event);
	}

	fn handle_get_versions_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		self.enqueue_response(
			*counterparty_node_id,
			request_id,
			LSPS2Response::GetVersions(GetVersionsResponse {
				versions: SUPPORTED_SPEC_VERSIONS.to_vec(),
			}),
		);
		Ok(())
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

	fn handle_get_info_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: GetInfoRequest,
	) -> Result<(), LightningError> {
		if !SUPPORTED_SPEC_VERSIONS.contains(&params.version) {
			self.enqueue_response(
				*counterparty_node_id,
				request_id,
				LSPS2Response::GetInfoError(ResponseError {
					code: LSPS2_GET_INFO_REQUEST_INVALID_VERSION_ERROR_CODE,
					message: format!("version {} is not supported", params.version),
					data: Some(format!("Supported versions are {:?}", SUPPORTED_SPEC_VERSIONS)),
				}),
			);
			return Err(LightningError {
				err: format!("client requested unsupported version {}", params.version),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			});
		}

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock: &mut Mutex<PeerState> =
			outer_state_lock.entry(*counterparty_node_id).or_insert(Mutex::new(PeerState::new()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock
			.pending_requests
			.insert(request_id.clone(), LSPS2Request::GetInfo(params.clone()));

		self.enqueue_event(Event::LSPS2(LSPS2Event::GetInfo {
			request_id,
			counterparty_node_id: *counterparty_node_id,
			version: params.version,
			token: params.token,
		}));
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

				self.enqueue_event(Event::LSPS2(LSPS2Event::GetInfoResponse {
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

	fn handle_buy_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: BuyRequest,
	) -> Result<(), LightningError> {
		if !SUPPORTED_SPEC_VERSIONS.contains(&params.version) {
			self.enqueue_response(
				*counterparty_node_id,
				request_id,
				LSPS2Response::BuyError(ResponseError {
					code: LSPS2_BUY_REQUEST_INVALID_VERSION_ERROR_CODE,
					message: format!("version {} is not supported", params.version),
					data: Some(format!("Supported versions are {:?}", SUPPORTED_SPEC_VERSIONS)),
				}),
			);
			return Err(LightningError {
				err: format!("client requested unsupported version {}", params.version),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			});
		}

		if let Some(payment_size_msat) = params.payment_size_msat {
			if payment_size_msat < self.min_payment_size_msat {
				self.enqueue_response(
					*counterparty_node_id,
					request_id,
					LSPS2Response::BuyError(ResponseError {
						code: LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_SMALL_ERROR_CODE,
						message: "payment size is below our minimum supported payment size"
							.to_string(),
						data: None,
					}),
				);
				return Err(LightningError {
					err: "payment size is below our minimum supported payment size".to_string(),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				});
			}

			if payment_size_msat > self.max_payment_size_msat {
				self.enqueue_response(
					*counterparty_node_id,
					request_id,
					LSPS2Response::BuyError(ResponseError {
						code: LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_LARGE_ERROR_CODE,
						message: "payment size is above our maximum supported payment size"
							.to_string(),
						data: None,
					}),
				);
				return Err(LightningError {
					err: "payment size is above our maximum supported payment size".to_string(),
					action: ErrorAction::IgnoreAndLog(Level::Info),
				});
			}

			match compute_opening_fee(
				payment_size_msat,
				params.opening_fee_params.min_fee_msat,
				params.opening_fee_params.proportional.into(),
			) {
				Some(opening_fee) => {
					if opening_fee >= payment_size_msat {
						self.enqueue_response(
							*counterparty_node_id,
							request_id,
							LSPS2Response::BuyError(ResponseError {
								code: LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_SMALL_ERROR_CODE,
								message: "payment size is too small to cover the opening fee"
									.to_string(),
								data: None,
							}),
						);
						return Err(LightningError {
							err: "payment size is too small to cover the opening fee".to_string(),
							action: ErrorAction::IgnoreAndLog(Level::Info),
						});
					}
				}
				None => {
					self.enqueue_response(
						*counterparty_node_id,
						request_id,
						LSPS2Response::BuyError(ResponseError {
							code: LSPS2_BUY_REQUEST_PAYMENT_SIZE_TOO_LARGE_ERROR_CODE,
							message: "overflow error when calculating opening_fee".to_string(),
							data: None,
						}),
					);
					return Err(LightningError {
						err: "overflow error when calculating opening_fee".to_string(),
						action: ErrorAction::IgnoreAndLog(Level::Info),
					});
				}
			}
		}

		// TODO: if payment_size_msat is specified, make sure our node has sufficient incoming liquidity from public network to receive it.

		if !is_valid_opening_fee_params(&params.opening_fee_params, &self.promise_secret) {
			self.enqueue_response(
				*counterparty_node_id,
				request_id,
				LSPS2Response::BuyError(ResponseError {
					code: LSPS2_BUY_REQUEST_INVALID_OPENING_FEE_PARAMS_ERROR_CODE,
					message: "valid_until is already past OR the promise did not match the provided parameters".to_string(),
					data: None,
				}),
			);
			return Err(LightningError {
				err: "invalid opening fee parameters were supplied by client".to_string(),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			});
		}

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock =
			outer_state_lock.entry(*counterparty_node_id).or_insert(Mutex::new(PeerState::new()));
		let mut peer_state_lock = inner_state_lock.lock().unwrap();
		peer_state_lock
			.pending_requests
			.insert(request_id.clone(), LSPS2Request::Buy(params.clone()));

		self.enqueue_event(Event::LSPS2(LSPS2Event::BuyRequest {
			request_id,
			version: params.version,
			counterparty_node_id: *counterparty_node_id,
			opening_fee_params: params.opening_fee_params,
			payment_size_msat: params.payment_size_msat,
		}));

		Ok(())
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
					self.enqueue_event(Event::LSPS2(LSPS2Event::InvoiceGenerationReady {
						counterparty_node_id: *counterparty_node_id,
						scid,
						cltv_expiry_delta: result.lsp_cltv_expiry_delta,
						payment_size_msat: jit_channel.config.payment_size_msat,
						client_trusts_lsp: result.client_trusts_lsp,
						user_channel_id: jit_channel.config.user_id,
					}));
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

impl<ES: Deref, CM: Deref + Clone, PM: Deref> ProtocolMessageHandler
	for LSPS2MessageHandler<ES, CM, PM>
where
	ES::Target: EntropySource,
	CM::Target: AChannelManager,
	PM::Target: APeerManager,
{
	type ProtocolMessage = LSPS2Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(2);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			LSPS2Message::Request(request_id, request) => match request {
				LSPS2Request::GetVersions(_) => {
					self.handle_get_versions_request(request_id, counterparty_node_id)
				}
				LSPS2Request::GetInfo(params) => {
					self.handle_get_info_request(request_id, counterparty_node_id, params)
				}
				LSPS2Request::Buy(params) => {
					self.handle_buy_request(request_id, counterparty_node_id, params)
				}
			},
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
		}
	}
}

fn calculate_amount_to_forward_per_htlc(
	htlcs: &[InterceptedHTLC], total_amt_to_forward_msat: u64,
) -> Vec<(InterceptId, u64)> {
	let total_received_msat: u64 =
		htlcs.iter().map(|htlc| htlc.expected_outbound_amount_msat).sum();

	let mut fee_remaining_msat = total_received_msat - total_amt_to_forward_msat;
	let total_fee_msat = fee_remaining_msat;

	let mut per_htlc_forwards = vec![];

	for (index, htlc) in htlcs.iter().enumerate() {
		let proportional_fee_amt_msat =
			total_fee_msat * htlc.expected_outbound_amount_msat / total_received_msat;

		let mut actual_fee_amt_msat = core::cmp::min(fee_remaining_msat, proportional_fee_amt_msat);
		fee_remaining_msat -= actual_fee_amt_msat;

		if index == htlcs.len() - 1 {
			actual_fee_amt_msat += fee_remaining_msat;
		}

		let amount_to_forward_msat = htlc.expected_outbound_amount_msat - actual_fee_amt_msat;

		per_htlc_forwards.push((htlc.intercept_id, amount_to_forward_msat))
	}

	per_htlc_forwards
}

#[cfg(test)]
mod tests {

	use super::*;

	#[test]
	fn test_calculate_amount_to_forward() {
		// TODO: Use proptest to generate random allocations
		let htlcs = vec![
			InterceptedHTLC {
				intercept_id: InterceptId([0; 32]),
				expected_outbound_amount_msat: 1000,
			},
			InterceptedHTLC {
				intercept_id: InterceptId([1; 32]),
				expected_outbound_amount_msat: 2000,
			},
			InterceptedHTLC {
				intercept_id: InterceptId([2; 32]),
				expected_outbound_amount_msat: 3000,
			},
		];

		let total_amt_to_forward_msat = 5000;

		let result = calculate_amount_to_forward_per_htlc(&htlcs, total_amt_to_forward_msat);

		assert_eq!(result[0].0, htlcs[0].intercept_id);
		assert_eq!(result[0].1, 834);

		assert_eq!(result[1].0, htlcs[1].intercept_id);
		assert_eq!(result[1].1, 1667);

		assert_eq!(result[2].0, htlcs[2].intercept_id);
		assert_eq!(result[2].1, 2499);
	}
}
