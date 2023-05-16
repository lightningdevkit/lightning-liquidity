// This file is Copyright its original authors, visible in version contror
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
use lightning::ln::peer_handler::{
	APeerManager, CustomMessageHandler, PeerManager, SocketDescriptor,
};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::Router;
use lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lightning::util::errors::APIError;
use lightning::util::logger::{Level, Logger};

use crate::events::EventQueue;
use crate::jit_channel::msgs::Message;
use crate::jit_channel::scid_utils;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::transport::msgs::{LSPSMessage, RequestId};
use crate::utils;
use crate::{events::Event, transport::msgs::ResponseError};

use super::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, GetVersionsRequest,
	GetVersionsResponse, OpeningFeeParams, RawOpeningFeeParams, Request, Response,
};

const SUPPORTED_SPEC_VERSION: u16 = 1;

#[derive(PartialEq)]
enum JITChannelState {
	VersionsRequested,
	MenuRequested,
	PendingMenuSelection,
	BuyRequested,
	PendingPayment,
	Ready,
}

struct JITChannel {
	id: u128,
	user_id: u128,
	state: JITChannelState,
	fees: Option<OpeningFeeParams>,
	token: Option<String>,
	min_payment_size_msat: Option<u64>,
	max_payment_size_msat: Option<u64>,
	payment_size_msat: Option<u64>,
	counterparty_node_id: PublicKey,
	amt_to_forward_msat: Option<u64>,
	intercept_id: Option<InterceptId>,
	scid: Option<u64>,
	lsp_cltv_expiry_delta: Option<u32>,
}

impl JITChannel {
	pub fn new(
		id: u128, counterparty_node_id: PublicKey, user_id: u128, payment_size_msat: Option<u64>,
		token: Option<String>,
	) -> Self {
		Self {
			id,
			counterparty_node_id,
			token,
			user_id,
			state: JITChannelState::VersionsRequested,
			fees: None,
			min_payment_size_msat: None,
			max_payment_size_msat: None,
			payment_size_msat,
			scid: None,
			amt_to_forward_msat: None,
			lsp_cltv_expiry_delta: None,
			intercept_id: None,
		}
	}
}

#[derive(Default)]
struct PeerState {
	channels_by_id: HashMap<u128, JITChannel>,
	request_to_cid: HashMap<RequestId, u128>,
	pending_requests: HashMap<RequestId, Request>,
}

impl PeerState {
	pub fn insert_channel(&mut self, channel_id: u128, channel: JITChannel) {
		self.channels_by_id.insert(channel_id, channel);
	}

	pub fn insert_request(&mut self, request_id: RequestId, channel_id: u128) {
		self.request_to_cid.insert(request_id, channel_id);
	}

	pub fn get_channel_in_state_for_request(
		&mut self, request_id: &RequestId, state: JITChannelState,
	) -> Option<&mut JITChannel> {
		let channel_id = self.request_to_cid.remove(request_id)?;

		if let Some(channel) = self.channels_by_id.get_mut(&channel_id) {
			if channel.state == state {
				return Some(channel);
			}
		}
		None
	}

	pub fn remove_channel(&mut self, channel_id: u128) {
		self.channels_by_id.remove(&channel_id);
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
	channels_by_scid: RwLock<HashMap<u64, JITChannel>>,
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
			channels_by_scid: RwLock::new(HashMap::new()),
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
		let channel_id = self.generate_channel_id();
		let channel = JITChannel::new(
			channel_id,
			counterparty_node_id,
			user_channel_id,
			payment_size_msat,
			token,
		);

		let mut per_peer_state = self.per_peer_state.write().unwrap();
		let peer_state_mutex =
			per_peer_state.entry(counterparty_node_id).or_insert(Mutex::new(PeerState::default()));
		let peer_state = peer_state_mutex.get_mut().unwrap();
		peer_state.insert_channel(channel_id, channel);

		let request_id = self.generate_request_id();
		peer_state.insert_request(request_id.clone(), channel_id);

		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages.push((
				counterparty_node_id,
				Message::Request(request_id, Request::GetVersions(GetVersionsRequest {})).into(),
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
		let per_peer_state = self.per_peer_state.read().unwrap();

		match per_peer_state.get(&counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state.pending_requests.remove(&request_id) {
					Some(Request::GetInfo(_)) => {
						let response = Response::GetInfo(GetInfoResponse {
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
		&self, counterparty_node_id: PublicKey, channel_id: u128,
		opening_fee_params: OpeningFeeParams,
	) -> Result<(), APIError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(&counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				if let Some(channel) = peer_state.channels_by_id.get_mut(&channel_id) {
					if channel.state == JITChannelState::PendingMenuSelection {
						channel.state = JITChannelState::BuyRequested;
						channel.fees = Some(opening_fee_params.clone());

						let request_id = self.generate_request_id();
						let payment_size_msat = channel.payment_size_msat;
						peer_state.insert_request(request_id.clone(), channel_id);

						{
							let mut pending_messages = self.pending_messages.lock().unwrap();
							pending_messages.push((
								counterparty_node_id,
								Message::Request(
									request_id,
									Request::Buy(BuyRequest {
										version: SUPPORTED_SPEC_VERSION,
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
							err: "Channel is not pending menu selection".to_string(),
						});
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

	pub fn invoice_parameters_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId, scid: u64,
		cltv_expiry_delta: u32, client_trusts_lsp: bool,
	) -> Result<(), APIError> {
		let per_peer_state = self.per_peer_state.read().unwrap();

		match per_peer_state.get(&counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state.pending_requests.remove(&request_id) {
					Some(Request::Buy(buy_request)) => {
						let mut channels_by_scid = self.channels_by_scid.write().unwrap();
						channels_by_scid.insert(
							scid,
							JITChannel {
								id: 0,
								user_id: 0,
								state: JITChannelState::BuyRequested,
								fees: Some(buy_request.opening_fee_params),
								token: None,
								min_payment_size_msat: None,
								max_payment_size_msat: None,
								payment_size_msat: buy_request.payment_size_msat,
								counterparty_node_id,
								scid: Some(scid),
								lsp_cltv_expiry_delta: Some(cltv_expiry_delta),
								amt_to_forward_msat: None,
								intercept_id: None,
							},
						);

						let block = scid_utils::block_from_scid(&scid);
						let tx_index = scid_utils::tx_index_from_scid(&scid);
						let vout = scid_utils::vout_from_scid(&scid);

						let jit_channel_scid = format!("{}x{}x{}", block, tx_index, vout);

						self.enqueue_response(
							counterparty_node_id,
							request_id,
							Response::Buy(BuyResponse {
								jit_channel_scid,
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

	// need to decide if we should ignore, enqueue OpenChannel event, or enqueue FailInterceptedHTLC event
	pub(crate) fn htlc_intercepted(
		&self, scid: u64, intercept_id: InterceptId, inbound_amount_msat: u64,
		expected_outbound_amount_msat: u64,
	) -> Result<(), APIError> {
		let mut channels_by_scid = self.channels_by_scid.write().unwrap();

		if let Some(channel) = channels_by_scid.get_mut(&scid) {
			if let Some(fees) = &channel.fees {
				let opening_fee_msat = utils::compute_opening_fee(
					expected_outbound_amount_msat,
					fees.min_fee_msat,
					fees.proportional as u64,
				);

				if let Some(opening_fee_msat) = opening_fee_msat {
					let amt_to_forward_msat = expected_outbound_amount_msat - opening_fee_msat;
					channel.amt_to_forward_msat = Some(amt_to_forward_msat);
					channel.intercept_id = Some(intercept_id);

					self.enqueue_event(Event::LSPS2(crate::JITChannelEvent::OpenChannel {
						their_network_key: channel.counterparty_node_id,
						inbound_amount_msat,
						expected_outbound_amount_msat,
						amt_to_forward_msat,
						opening_fee_msat,
						user_channel_id: scid as u128,
					}));
				} else {
					self.channel_manager.fail_intercepted_htlc(intercept_id)?;
				}
			} else {
				self.channel_manager.fail_intercepted_htlc(intercept_id)?;
			}
		}

		Ok(())
	}

	// figure out which intercept id is waiting on this channel and enqueue ForwardInterceptedHTLC event
	pub(crate) fn channel_ready(
		&self, user_channel_id: u128, channel_id: &[u8; 32], counterparty_node_id: &PublicKey,
	) -> Result<(), APIError> {
		let channels_by_scid = self.channels_by_scid.read().unwrap();

		if let Ok(scid) = user_channel_id.try_into() {
			if let Some(channel) = channels_by_scid.get(&scid) {
				self.channel_manager.forward_intercepted_htlc(
					channel.intercept_id.unwrap(),
					channel_id,
					*counterparty_node_id,
					channel.amt_to_forward_msat.unwrap(),
				)?;
			} else {
				return Err(APIError::APIMisuseError {
					err: format!(
						"Could not find a channel with user_channel_id {}",
						user_channel_id
					),
				});
			}
		} else {
			return Err(APIError::APIMisuseError {
				err: format!("Could not parse user_channel_id into u64 scid {}", user_channel_id),
			});
		}

		Ok(())
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

	fn enqueue_response(
		&self, counterparty_node_id: PublicKey, request_id: RequestId, response: Response,
	) {
		{
			let mut pending_messages = self.pending_messages.lock().unwrap();
			pending_messages
				.push((counterparty_node_id, Message::Response(request_id, response).into()));
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
		// not sure best way to extract a vec to a constant? lazy_static?
		self.enqueue_response(
			*counterparty_node_id,
			request_id,
			Response::GetVersions(GetVersionsResponse { versions: vec![1] }),
		);
		Ok(())
	}

	fn handle_get_versions_response(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, result: GetVersionsResponse,
	) -> Result<(), LightningError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state.get_channel_in_state_for_request(
					&request_id,
					JITChannelState::VersionsRequested,
				) {
					Some(channel) => {
						let channel_id = channel.id;
						let token = channel.token.clone();

						if result.versions.contains(&SUPPORTED_SPEC_VERSION) {
							channel.state = JITChannelState::MenuRequested;

							let request_id = self.generate_request_id();
							peer_state.insert_request(request_id.clone(), channel_id);

							{
								let mut pending_messages = self.pending_messages.lock().unwrap();
								pending_messages.push((
									*counterparty_node_id,
									Message::Request(
										request_id,
										Request::GetInfo(GetInfoRequest {
											version: SUPPORTED_SPEC_VERSION,
											token,
										}),
									)
									.into(),
								));
							}

							if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
								peer_manager.process_events();
							}
						} else {
							peer_state.remove_channel(channel_id);
						}
					}
					None => {
						return Err(LightningError {
							err: format!(
								"Received get_versions response without a matching channel: {:?}",
								request_id
							),
							action: ErrorAction::IgnoreAndLog(Level::Info),
						})
					}
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
		let mut per_peer_state = self.per_peer_state.write().unwrap();
		let peer_state_mutex: &mut Mutex<PeerState> =
			per_peer_state.entry(*counterparty_node_id).or_insert(Mutex::new(PeerState::default()));
		let peer_state = peer_state_mutex.get_mut().unwrap();
		peer_state.pending_requests.insert(request_id.clone(), Request::GetInfo(params.clone()));

		self.enqueue_event(Event::LSPS2(super::event::Event::GetInfo {
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
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::MenuRequested)
				{
					Some(channel) => {
						channel.state = JITChannelState::PendingMenuSelection;
						channel.min_payment_size_msat = Some(result.min_payment_size_msat);
						channel.max_payment_size_msat = Some(result.max_payment_size_msat);

						self.enqueue_event(Event::LSPS2(super::event::Event::GetInfoResponse {
							counterparty_node_id: *counterparty_node_id,
							opening_fee_params_menu: result.opening_fee_params_menu,
							min_payment_size_msat: result.min_payment_size_msat,
							max_payment_size_msat: result.max_payment_size_msat,
							channel_id: channel.id,
							user_channel_id: channel.user_id,
						}));
					}
					None => {
						return Err(LightningError {
							err: format!(
								"Received get_info response without a matching channel: {:?}",
								request_id
							),
							action: ErrorAction::IgnoreAndLog(Level::Info),
						})
					}
				}
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
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, error: ResponseError,
	) -> Result<(), LightningError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::BuyRequested)
				{
					Some(channel) => {
						let channel_id = channel.id;
						peer_state.remove_channel(channel_id);
						return Err(LightningError {
							err: format!("Received error response from getinfo request ({:?}) with counterparty {:?}.  Removing channel {}. code = {}, message = {}", request_id, counterparty_node_id, channel_id, error.code, error.message),
							action: ErrorAction::IgnoreAndLog(Level::Info)
						});
					}
					None => {
						return Err(LightningError {
							err: format!("Received an unexpected error response for a getinfo request from counterparty ({:?})", counterparty_node_id),
							action: ErrorAction::IgnoreAndLog(Level::Info)
						});
					}
				}
			}
			None => {
				return Err(LightningError {
					err: format!("Received error response for a getinfo request from an unknown counterparty ({:?})", counterparty_node_id),
					action: ErrorAction::IgnoreAndLog(Level::Info)
				});
			}
		}
	}

	fn handle_buy_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: BuyRequest,
	) -> Result<(), LightningError> {
		if params.opening_fee_params.is_valid(&self.promise_secret) {
			let mut per_peer_state = self.per_peer_state.write().unwrap();
			let peer_state_mutex = per_peer_state
				.entry(*counterparty_node_id)
				.or_insert(Mutex::new(PeerState::default()));
			let peer_state = peer_state_mutex.get_mut().unwrap();
			peer_state.pending_requests.insert(request_id.clone(), Request::Buy(params.clone()));

			self.enqueue_event(Event::LSPS2(super::event::Event::BuyRequest {
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
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::BuyRequested)
				{
					Some(channel) => {
						channel.state = JITChannelState::PendingPayment;
						channel.lsp_cltv_expiry_delta = Some(result.lsp_cltv_expiry_delta);

						if let Ok(scid) =
							scid_utils::scid_from_human_readable_string(&result.jit_channel_scid)
						{
							channel.scid = Some(scid);

							self.enqueue_event(Event::LSPS2(
								super::event::Event::InvoiceGenerationReady {
									counterparty_node_id: *counterparty_node_id,
									scid,
									cltv_expiry_delta: result.lsp_cltv_expiry_delta,
									min_payment_size_msat: channel.min_payment_size_msat,
									max_payment_size_msat: channel.max_payment_size_msat,
									payment_size_msat: channel.payment_size_msat,
									fees: channel.fees.clone().unwrap(),
									client_trusts_lsp: result.client_trusts_lsp,
									user_channel_id: channel.user_id,
								},
							));
						} else {
							return Err(LightningError {
								err: format!(
									"Received buy response with an invalid scid {}",
									result.jit_channel_scid
								),
								action: ErrorAction::IgnoreAndLog(Level::Info),
							});
						}
					}
					None => {
						return Err(LightningError {
							err: format!(
								"Received buy response without a matching channel: {:?}",
								request_id
							),
							action: ErrorAction::IgnoreAndLog(Level::Info),
						});
					}
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
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, error: ResponseError,
	) -> Result<(), LightningError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::BuyRequested)
				{
					Some(channel) => {
						let channel_id = channel.id;
						peer_state.remove_channel(channel_id);
						return Err(LightningError { err: format!( "Received error response from buy request ({:?}) with counterparty {:?}.  Removing channel {}. code = {}, message = {}", request_id, counterparty_node_id, channel_id, error.code, error.message), action: ErrorAction::IgnoreAndLog(Level::Info)});
					}
					None => {
						return Err(LightningError { err: format!("Received an unexpected error response for a buy request from counterparty ({:?})", counterparty_node_id), action: ErrorAction::IgnoreAndLog(Level::Info)});
					}
				}
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
	type ProtocolMessage = Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(2);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), LightningError> {
		match message {
			Message::Request(request_id, request) => match request {
				super::msgs::Request::GetVersions(_) => {
					self.handle_get_versions_request(request_id, counterparty_node_id)
				}
				super::msgs::Request::GetInfo(params) => {
					self.handle_get_info_request(request_id, counterparty_node_id, params)
				}
				super::msgs::Request::Buy(params) => {
					self.handle_buy_request(request_id, counterparty_node_id, params)
				}
			},
			Message::Response(request_id, response) => match response {
				super::msgs::Response::GetVersions(result) => {
					self.handle_get_versions_response(request_id, counterparty_node_id, result)
				}
				super::msgs::Response::GetInfo(result) => {
					self.handle_get_info_response(request_id, counterparty_node_id, result)
				}
				super::msgs::Response::GetInfoError(error) => {
					self.handle_get_info_error(request_id, counterparty_node_id, error)
				}
				super::msgs::Response::Buy(result) => {
					self.handle_buy_response(request_id, counterparty_node_id, result)
				}
				super::msgs::Response::BuyError(error) => {
					self.handle_buy_error(request_id, counterparty_node_id, error)
				}
			},
		}
	}
}
