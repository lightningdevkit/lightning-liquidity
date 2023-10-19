// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

use std::collections::HashMap;
use std::convert::TryInto;
use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};

use bitcoin::secp256k1::PublicKey;
use lightning::chain;
use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::ln::channelmanager::{ChannelManager, InterceptId};
use lightning::ln::msgs::{
	ChannelMessageHandler, ErrorAction, LightningError, OnionMessageHandler, RoutingMessageHandler,
};
use lightning::ln::peer_handler::{CustomMessageHandler, PeerManager, SocketDescriptor};
use lightning::ln::ChannelId;
use lightning::routing::router::Router;
use lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lightning::util::errors::APIError;
use lightning::util::logger::{Level, Logger};

use crate::events::EventQueue;
use crate::jit_channel::utils::{compute_opening_fee, is_valid_opening_fee_params};
use crate::jit_channel::LSPS2Event;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::transport::msgs::{LSPSMessage, RequestId};
use crate::utils;
use crate::{events::Event, transport::msgs::ResponseError};

use crate::jit_channel::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, GetVersionsRequest,
	GetVersionsResponse, JitChannelScid, LSPS2Message, LSPS2Request, LSPS2Response,
	OpeningFeeParams, RawOpeningFeeParams,
};

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
	PendingPayment { client_trusts_lsp: bool, short_channel_id: JitChannelScid },
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
		&self, client_trusts_lsp: bool, short_channel_id: JitChannelScid,
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
	pub fn new(
		id: u128, user_id: u128, payment_size_msat: Option<u64>, token: Option<String>,
	) -> Self {
		Self {
			id,
			config: InboundJITChannelConfig { user_id, payment_size_msat, token },
			state: InboundJITChannelState::VersionsRequested,
		}
	}

	pub fn versions_received(&mut self, versions: Vec<u16>) -> Result<u16, LightningError> {
		self.state = self.state.versions_received(versions)?;

		match self.state {
			InboundJITChannelState::MenuRequested { version } => Ok(version),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	pub fn info_received(&mut self) -> Result<(), LightningError> {
		self.state = self.state.info_received()?;
		Ok(())
	}

	pub fn opening_fee_params_selected(&mut self) -> Result<u16, LightningError> {
		self.state = self.state.opening_fee_params_selected()?;

		match self.state {
			InboundJITChannelState::BuyRequested { version } => Ok(version),
			_ => Err(LightningError {
				action: ErrorAction::IgnoreAndLog(Level::Error),
				err: "impossible state transition".to_string(),
			}),
		}
	}

	pub fn invoice_params_received(
		&mut self, client_trusts_lsp: bool, jit_channel_scid: JitChannelScid,
	) -> Result<(), LightningError> {
		self.state = self.state.invoice_params_received(client_trusts_lsp, jit_channel_scid)?;
		Ok(())
	}
}

#[derive(PartialEq, Debug)]
enum OutboundJITChannelState {
	InvoiceParametersGenerated {
		short_channel_id: u64,
		cltv_expiry_delta: u32,
		payment_size_msat: Option<u64>,
		opening_fee_params: OpeningFeeParams,
	},
	PendingChannelOpen {
		intercept_id: InterceptId,
		opening_fee_msat: u64,
		amt_to_forward_msat: u64,
	},
	ChannelReady {
		intercept_id: InterceptId,
		amt_to_forward_msat: u64,
	},
}

impl OutboundJITChannelState {
	pub fn new(
		short_channel_id: u64, cltv_expiry_delta: u32, payment_size_msat: Option<u64>,
		opening_fee_params: OpeningFeeParams,
	) -> Self {
		OutboundJITChannelState::InvoiceParametersGenerated {
			short_channel_id,
			cltv_expiry_delta,
			payment_size_msat,
			opening_fee_params,
		}
	}

	pub fn htlc_intercepted(
		&self, expected_outbound_amount_msat: u64, intercept_id: InterceptId,
	) -> Result<Self, ChannelStateError> {
		match self {
			OutboundJITChannelState::InvoiceParametersGenerated { opening_fee_params, .. } => {
				compute_opening_fee(
					expected_outbound_amount_msat,
					opening_fee_params.min_fee_msat,
					opening_fee_params.proportional,
				).map(|opening_fee_msat| OutboundJITChannelState::PendingChannelOpen {
					intercept_id,
					opening_fee_msat,
					amt_to_forward_msat: expected_outbound_amount_msat - opening_fee_msat,
				}).ok_or(ChannelStateError(format!("Could not compute valid opening fee with min_fee_msat = {}, proportional = {}, and expected_outbound_amount_msat = {}", opening_fee_params.min_fee_msat, opening_fee_params.proportional, expected_outbound_amount_msat)))
			}
			state => Err(ChannelStateError(format!(
				"Invoice params received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}

	pub fn channel_ready(&self) -> Result<Self, ChannelStateError> {
		match self {
			OutboundJITChannelState::PendingChannelOpen {
				intercept_id,
				amt_to_forward_msat,
				..
			} => Ok(OutboundJITChannelState::ChannelReady {
				intercept_id: *intercept_id,
				amt_to_forward_msat: *amt_to_forward_msat,
			}),
			state => Err(ChannelStateError(format!(
				"Channel ready received when JIT Channel was in state: {:?}",
				state
			))),
		}
	}
}

struct OutboundJITChannel {
	state: OutboundJITChannelState,
}

impl OutboundJITChannel {
	pub fn new(
		scid: u64, cltv_expiry_delta: u32, payment_size_msat: Option<u64>,
		opening_fee_params: OpeningFeeParams,
	) -> Self {
		Self {
			state: OutboundJITChannelState::new(
				scid,
				cltv_expiry_delta,
				payment_size_msat,
				opening_fee_params,
			),
		}
	}

	pub fn htlc_intercepted(
		&mut self, expected_outbound_amount_msat: u64, intercept_id: InterceptId,
	) -> Result<(u64, u64), LightningError> {
		self.state = self.state.htlc_intercepted(expected_outbound_amount_msat, intercept_id)?;

		match &self.state {
			OutboundJITChannelState::PendingChannelOpen {
				opening_fee_msat,
				amt_to_forward_msat,
				..
			} => Ok((*opening_fee_msat, *amt_to_forward_msat)),
			impossible_state => Err(LightningError {
				err: format!(
					"Impossible state transition during htlc_intercepted to {:?}",
					impossible_state
				),
				action: ErrorAction::IgnoreAndLog(Level::Info),
			}),
		}
	}

	pub fn channel_ready(&mut self) -> Result<(InterceptId, u64), LightningError> {
		self.state = self.state.channel_ready()?;

		match &self.state {
			OutboundJITChannelState::ChannelReady { intercept_id, amt_to_forward_msat } => {
				Ok((*intercept_id, *amt_to_forward_msat))
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

#[derive(Default)]
struct PeerState {
	inbound_channels_by_id: HashMap<u128, InboundJITChannel>,
	outbound_channels_by_scid: HashMap<u64, OutboundJITChannel>,
	request_to_cid: HashMap<RequestId, u128>,
	pending_requests: HashMap<RequestId, LSPS2Request>,
}

impl PeerState {
	pub fn insert_inbound_channel(&mut self, jit_channel_id: u128, channel: InboundJITChannel) {
		self.inbound_channels_by_id.insert(jit_channel_id, channel);
	}

	pub fn insert_outbound_channel(&mut self, scid: u64, channel: OutboundJITChannel) {
		self.outbound_channels_by_scid.insert(scid, channel);
	}

	pub fn insert_request(&mut self, request_id: RequestId, jit_channel_id: u128) {
		self.request_to_cid.insert(request_id, jit_channel_id);
	}

	pub fn remove_inbound_channel(&mut self, jit_channel_id: u128) {
		self.inbound_channels_by_id.remove(&jit_channel_id);
	}

	pub fn remove_outbound_channel(&mut self, scid: u64) {
		self.outbound_channels_by_scid.remove(&scid);
	}
}

pub struct JITChannelManager<
	ES: Deref,
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
	CMH: Deref,
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
	CMH::Target: CustomMessageHandler,
	NS::Target: NodeSigner,
{
	entropy_source: ES,
	peer_manager: Mutex<Option<Arc<PeerManager<Descriptor, CM, RM, OM, L, CMH, NS>>>>,
	channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
	pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
	pending_events: Arc<EventQueue>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	peer_by_scid: RwLock<HashMap<u64, PublicKey>>,
	promise_secret: [u8; 32],
}

impl<
		ES: Deref,
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
		CMH: Deref,
		NS: Deref,
	> JITChannelManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, CMH, NS>
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
	CMH::Target: CustomMessageHandler,
	NS::Target: NodeSigner,
{
	pub(crate) fn new(
		entropy_source: ES, promise_secret: [u8; 32],
		pending_messages: Arc<Mutex<Vec<(PublicKey, LSPSMessage)>>>,
		pending_events: Arc<EventQueue>,
		channel_manager: Arc<ChannelManager<M, T, ES, NS, SP, F, R, L>>,
	) -> Self {
		Self {
			entropy_source,
			promise_secret,
			pending_messages,
			pending_events,
			per_peer_state: RwLock::new(HashMap::new()),
			peer_by_scid: RwLock::new(HashMap::new()),
			peer_manager: Mutex::new(None),
			channel_manager,
		}
	}

	pub fn set_peer_manager(
		&self, peer_manager: Arc<PeerManager<Descriptor, CM, RM, OM, L, CMH, NS>>,
	) {
		*self.peer_manager.lock().unwrap() = Some(peer_manager);
	}

	pub fn create_invoice(
		&self, counterparty_node_id: PublicKey, payment_size_msat: Option<u64>,
		token: Option<String>, user_channel_id: u128,
	) {
		let jit_channel_id = self.generate_jit_channel_id();
		let channel =
			InboundJITChannel::new(jit_channel_id, user_channel_id, payment_size_msat, token);

		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock = outer_state_lock
			.entry(counterparty_node_id)
			.or_insert(Mutex::new(PeerState::default()));
		let peer_state = inner_state_lock.get_mut().unwrap();
		peer_state.insert_inbound_channel(jit_channel_id, channel);

		let request_id = self.generate_request_id();
		peer_state.insert_request(request_id.clone(), jit_channel_id);

		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages.push((
				counterparty_node_id,
				LSPS2Message::Request(request_id, LSPS2Request::GetVersions(GetVersionsRequest {}))
					.into(),
			));
		}

		if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
			peer_manager.process_events();
		}
	}

	pub fn opening_fee_params_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
		opening_fee_params_menu: Vec<RawOpeningFeeParams>, min_payment_size_msat: u64,
		max_payment_size_msat: u64,
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
							min_payment_size_msat,
							max_payment_size_msat,
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
						peer_manager.process_events();
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

	pub(crate) fn htlc_intercepted(
		&self, scid: u64, intercept_id: InterceptId, inbound_amount_msat: u64,
		expected_outbound_amount_msat: u64,
	) -> Result<(), APIError> {
		let peer_by_scid = self.peer_by_scid.read().unwrap();
		if let Some(counterparty_node_id) = peer_by_scid.get(&scid) {
			let outer_state_lock = self.per_peer_state.read().unwrap();
			match outer_state_lock.get(counterparty_node_id) {
				Some(inner_state_lock) => {
					let mut peer_state = inner_state_lock.lock().unwrap();
					if let Some(jit_channel) = peer_state.outbound_channels_by_scid.get_mut(&scid) {
						// TODO: Need to support MPP payments. If payment_amount_msat is known, needs to queue intercepted HTLCs in a map by payment_hash
						//       LiquidityManager will need to be regularly polled so it can continually check if the payment amount has been received
						//       and can release the payment or if the channel valid_until has expired and should be failed.
						//       Can perform check each time HTLC is received and on interval? I guess interval only needs to check expiration as
						//       we can only reach threshold when htlc is intercepted.

						match jit_channel
							.htlc_intercepted(expected_outbound_amount_msat, intercept_id)
						{
							Ok((opening_fee_msat, amt_to_forward_msat)) => {
								self.enqueue_event(Event::LSPS2(LSPS2Event::OpenChannel {
									their_network_key: counterparty_node_id.clone(),
									inbound_amount_msat,
									expected_outbound_amount_msat,
									amt_to_forward_msat,
									opening_fee_msat,
									user_channel_id: scid as u128,
								}));
							}
							Err(e) => {
								self.channel_manager.fail_intercepted_htlc(intercept_id)?;
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

	// figure out which intercept id is waiting on this channel and enqueue ForwardInterceptedHTLC event
	pub(crate) fn channel_ready(
		&self, user_channel_id: u128, channel_id: &ChannelId, counterparty_node_id: &PublicKey,
	) -> Result<(), APIError> {
		if let Ok(scid) = user_channel_id.try_into() {
			let outer_state_lock = self.per_peer_state.read().unwrap();
			match outer_state_lock.get(counterparty_node_id) {
				Some(inner_state_lock) => {
					let mut peer_state = inner_state_lock.lock().unwrap();
					if let Some(jit_channel) = peer_state.outbound_channels_by_scid.get_mut(&scid) {
						match jit_channel.channel_ready() {
							Ok((intercept_id, amt_to_forward_msat)) => {
								self.channel_manager.forward_intercepted_htlc(
									intercept_id,
									channel_id,
									*counterparty_node_id,
									amt_to_forward_msat,
								)?;
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
			peer_manager.process_events();
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
							jit_channel_id
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
					peer_manager.process_events();
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
		let mut outer_state_lock = self.per_peer_state.write().unwrap();
		let inner_state_lock: &mut Mutex<PeerState> = outer_state_lock
			.entry(*counterparty_node_id)
			.or_insert(Mutex::new(PeerState::default()));
		let peer_state = inner_state_lock.get_mut().unwrap();
		peer_state
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
					jit_channel_id: jit_channel.id,
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
		// TODO: need to perform check on `params.version`.
		// TODO: if payment_size_msat is specified, make sure opening_fee is >= payment_size_msat.
		// TODO: if payment_size_msat is specified, make sure opening_fee does not hit overflow error.
		// TODO: if payment_size_msat is specified, make sure our node has sufficient incoming liquidity from public network to receive it.

		if is_valid_opening_fee_params(&params.opening_fee_params, &self.promise_secret) {
			let mut outer_state_lock = self.per_peer_state.write().unwrap();
			let inner_state_lock = outer_state_lock
				.entry(*counterparty_node_id)
				.or_insert(Mutex::new(PeerState::default()));
			let peer_state = inner_state_lock.get_mut().unwrap();
			peer_state
				.pending_requests
				.insert(request_id.clone(), LSPS2Request::Buy(params.clone()));

			self.enqueue_event(Event::LSPS2(LSPS2Event::BuyRequest {
				request_id,
				version: params.version,
				counterparty_node_id: *counterparty_node_id,
				opening_fee_params: params.opening_fee_params,
				payment_size_msat: params.payment_size_msat,
			}));
		}
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

impl<
		ES: Deref,
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
		CMH: Deref,
		NS: Deref,
	> ProtocolMessageHandler
	for JITChannelManager<ES, M, T, F, R, SP, Descriptor, L, RM, CM, OM, CMH, NS>
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
	CMH::Target: CustomMessageHandler,
	NS::Target: NodeSigner,
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
