use crate::jit_channel;
use bitcoin::secp256k1::PublicKey;

pub trait EventHandler {
	fn handle_event(&self, event: Event);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
	ListProtocols { counterparty_node_id: PublicKey, protocols: Vec<u16> },
	LSPS2(jit_channel::event::Event),
}
