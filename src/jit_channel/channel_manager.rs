// This file is Copyright its original authors, visible in version contror
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Mutex, RwLock};

use bitcoin::secp256k1::PublicKey;
use lightning::chain::keysinterface::EntropySource;
use lightning::util::errors::APIError;
use lightning::util::logger::Logger;
use lightning::{log_error, log_info};

use crate::jit_channel::msgs::Message;
use crate::jit_channel::scid_utils;
use crate::transport::message_handler::ProtocolMessageHandler;
use crate::transport::msgs::RequestId;
use crate::utils;
use crate::{events::Event, transport::msgs::ResponseError};

use super::msgs::{
	BuyRequest, BuyResponse, GetInfoRequest, GetInfoResponse, GetVersionsRequest,
	GetVersionsResponse, OpeningFeeParams, RawOpeningFeeParams, Request, Response,
};

const SUPPORTED_SPEC_VERSION: u16 = 1;

#[derive(PartialEq)]
enum JITChannelState {
	New,
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
	min_payment_size_msat: Option<u64>,
	max_payment_size_msat: Option<u64>,
	payment_size_msat: Option<u64>,
	counterparty_node_id: PublicKey,
	scid: Option<u64>,
	lsp_cltv_expiry_delta: Option<u32>,
}

impl JITChannel {
	pub fn new(
		id: u128, counterparty_node_id: PublicKey, user_id: Option<u128>,
		payment_size_msat: Option<u64>,
	) -> Self {
		Self {
			id,
			counterparty_node_id,
			user_id: user_id.unwrap_or(0),
			state: JITChannelState::New,
			fees: None,
			min_payment_size_msat: None,
			max_payment_size_msat: None,
			payment_size_msat,
			scid: None,
			lsp_cltv_expiry_delta: None,
		}
	}
}

#[derive(Default)]
struct PeerState {
	channels_by_id: HashMap<u128, JITChannel>,
	request_to_cid: HashMap<RequestId, u128>,
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

pub struct JITChannelManager<L: Deref, ES: Deref>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	logger: L,
	entropy_source: ES,
	pending_messages: Mutex<Vec<(PublicKey, Message)>>,
	pending_events: Mutex<Vec<Event>>,
	per_peer_state: RwLock<HashMap<PublicKey, Mutex<PeerState>>>,
	promise_secret: [u8; 32],
}

impl<L: Deref, ES: Deref> JITChannelManager<L, ES>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	pub fn create_invoice(
		&self, counterparty_node_id: PublicKey, payment_size_msat: Option<u64>,
	) -> Result<(), APIError> {
		let channel_id = self.generate_channel_id();
		let channel = JITChannel::new(channel_id, counterparty_node_id, None, payment_size_msat);

		let mut per_peer_state = self.per_peer_state.write().unwrap();
		let peer_state_mutex =
			per_peer_state.entry(counterparty_node_id).or_insert(Mutex::new(PeerState::default()));
		let peer_state = peer_state_mutex.get_mut().unwrap();
		peer_state.insert_channel(channel_id, channel);

		let request_id = self.generate_request_id();
		peer_state.insert_request(request_id.clone(), channel_id);

		let mut pending_messages = self.pending_messages.lock().unwrap();
		pending_messages.push((
			counterparty_node_id,
			Message::Request(request_id, Request::GetVersions(GetVersionsRequest {})),
		));
		Ok(())
	}

	/// LSP provides the menu and payment size limits in response to a GetInfo request from a client.
	pub fn opening_fee_params_generated(
		&self, counterparty_node_id: PublicKey, request_id: RequestId,
		opening_fee_params_menu: Vec<RawOpeningFeeParams>, min_payment_size_msat: u64,
		max_payment_size_msat: u64,
	) -> Result<(), APIError> {
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
							),
						));
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
		cltv_expiry_delta: u32,
	) -> Result<(), APIError> {
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
			}),
		);
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
		let mut pending_messages = self.pending_messages.lock().unwrap();
		pending_messages.push((counterparty_node_id, Message::Response(request_id, response)));
	}

	fn enqueue_event(&self, event: Event) {
		// if we're really going to have specific Event's to each LSPS then maybe we want to add support for a ProtocolEvent
		// in addition to the ProtocolMessage and require Into/From implementations
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.push(event);
	}

	fn handle_get_versions_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
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
	) -> Result<(), lightning::ln::msgs::LightningError> {
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

						if result.versions.contains(&SUPPORTED_SPEC_VERSION) {
							channel.state = JITChannelState::MenuRequested;

							let request_id = self.generate_request_id();
							peer_state.insert_request(request_id.clone(), channel_id);

							let mut pending_messages = self.pending_messages.lock().unwrap();
							pending_messages.push((
								*counterparty_node_id,
								Message::Request(
									request_id,
									Request::GetInfo(GetInfoRequest {
										version: SUPPORTED_SPEC_VERSION,
										token: None,
									}),
								),
							));
						} else {
							peer_state.remove_channel(channel_id);
						}
					}
					None => {
						log_error!(
							self.logger,
							"Received get_versions response without a matching channel: {:?}",
							request_id
						)
					}
				}
			}
			None => {
				log_error!(
					self.logger,
					"Received get_versions response from unknown peer: {:?}",
					counterparty_node_id
				);
			}
		}

		Ok(())
	}

	fn handle_get_info_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: GetInfoRequest,
	) -> Result<(), lightning::ln::msgs::LightningError> {
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
	) -> Result<(), lightning::ln::msgs::LightningError> {
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
						}));
					}
					None => {
						log_error!(
							self.logger,
							"Received get_info response without a matching channel: {:?}",
							request_id
						)
					}
				}
			}
			None => {
				log_error!(
					self.logger,
					"Received get_info response from unknown peer: {:?}",
					counterparty_node_id
				);
			}
		}

		Ok(())
	}

	fn handle_get_info_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, error: ResponseError,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::BuyRequested)
				{
					Some(channel) => {
						let channel_id = channel.id;
						log_error!(self.logger, "Received error response from getinfo request ({:?}) with counterparty {:?}.  Removing channel {}. code = {}, message = {}", request_id, counterparty_node_id, channel_id, error.code, error.message);
						peer_state.remove_channel(channel_id);
					}
					None => {
						log_info!(self.logger, "Received an unexpected error response for a getinfo request from counterparty ({:?})", counterparty_node_id);
					}
				}
			}
			None => {
				log_info!(self.logger, "Received error response for a getinfo request from an unknown counterparty ({:?})", counterparty_node_id);
			}
		}
		Ok(())
	}

	fn handle_buy_request(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, params: BuyRequest,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		if params.opening_fee_params.is_valid(&self.promise_secret) {
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
	) -> Result<(), lightning::ln::msgs::LightningError> {
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
								},
							));
						} else {
							log_error!(
								self.logger,
								"Received buy response with an invalid scid {}",
								result.jit_channel_scid
							)
						}
					}
					None => {
						log_error!(
							self.logger,
							"Received buy response without a matching channel: {:?}",
							request_id
						)
					}
				}
			}
			None => {
				log_error!(
					self.logger,
					"Received buy response from unknown peer: {:?}",
					counterparty_node_id
				);
			}
		}
		Ok(())
	}

	fn handle_buy_error(
		&self, request_id: RequestId, counterparty_node_id: &PublicKey, error: ResponseError,
	) -> Result<(), lightning::ln::msgs::LightningError> {
		let per_peer_state = self.per_peer_state.read().unwrap();
		match per_peer_state.get(counterparty_node_id) {
			Some(peer_state_mutex) => {
				let mut peer_state = peer_state_mutex.lock().unwrap();

				match peer_state
					.get_channel_in_state_for_request(&request_id, JITChannelState::BuyRequested)
				{
					Some(channel) => {
						let channel_id = channel.id;
						log_error!(self.logger, "Received error response from buy request ({:?}) with counterparty {:?}.  Removing channel {}. code = {}, message = {}", request_id, counterparty_node_id, channel_id, error.code, error.message);
						peer_state.remove_channel(channel_id);
					}
					None => {
						log_info!(self.logger, "Received an unexpected error response for a buy request from counterparty ({:?})", counterparty_node_id);
					}
				}
			}
			None => {
				log_info!(
					self.logger,
					"Received error response for a buy request from an unknown counterparty ({:?})",
					counterparty_node_id
				);
			}
		}

		Ok(())
	}
}

impl<L: Deref, ES: Deref> ProtocolMessageHandler for JITChannelManager<L, ES>
where
	L::Target: Logger,
	ES::Target: EntropySource,
{
	type ProtocolMessage = Message;
	const PROTOCOL_NUMBER: Option<u16> = Some(2);

	fn handle_message(
		&self, message: Self::ProtocolMessage, counterparty_node_id: &PublicKey,
	) -> Result<(), lightning::ln::msgs::LightningError> {
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

	fn get_and_clear_pending_protocol_messages(&self) -> Vec<(PublicKey, Self::ProtocolMessage)> {
		let mut pending_messages = self.pending_messages.lock().unwrap();
		pending_messages.drain(..).collect()
	}

	fn get_and_clear_pending_protocol_events(&self) -> Vec<crate::events::Event> {
		let mut pending_events = self.pending_events.lock().unwrap();
		pending_events.drain(..).collect()
	}
}
