//! Holds types and traits used to implement message queues for [`LSPSMessage`]s.

use crate::lsps0::msgs::LSPSMessage;
use crate::prelude::{Vec, VecDeque};
use crate::sync::Mutex;

use lightning::ln::peer_handler::APeerManager;

use bitcoin::secp256k1::PublicKey;

use core::ops::Deref;

/// Represents a simple message queue that the LSPS message handlers use to send messages to a given counterparty.
pub trait MessageQueue {
	/// Enqueues an LSPS message to be sent to the counterparty with the given node id.
	///
	/// Implementations need to take care of message delivery to the counterparty via the
	/// LSPS0/BOLT8 transport protocol.
	fn enqueue(&self, counterparty_node_id: &PublicKey, msg: LSPSMessage);
}

/// The default [`MessageQueue`] Implementation used by [`LiquidityManager`].
///
/// [`LiquidityManager`]: crate::LiquidityManager
pub struct DefaultMessageQueue<PM: Deref>
where
	PM::Target: APeerManager,
{
	queue: Mutex<VecDeque<(PublicKey, LSPSMessage)>>,
	peer_manager: Mutex<Option<PM>>,
}

impl<PM: Deref> DefaultMessageQueue<PM>
where
	PM::Target: APeerManager,
{
	pub(crate) fn new() -> Self {
		let queue = Mutex::new(VecDeque::new());
		let peer_manager = Mutex::new(None);
		Self { queue, peer_manager }
	}

	pub(crate) fn get_and_clear_pending_msgs(&self) -> Vec<(PublicKey, LSPSMessage)> {
		self.queue.lock().unwrap().drain(..).collect()
	}

	pub(crate) fn set_peer_manager(&self, peer_manager: PM) {
		*self.peer_manager.lock().unwrap() = Some(peer_manager);
	}
}

impl<PM: Deref> MessageQueue for DefaultMessageQueue<PM>
where
	PM::Target: APeerManager,
{
	fn enqueue(&self, counterparty_node_id: &PublicKey, msg: LSPSMessage) {
		{
			let mut queue = self.queue.lock().unwrap();
			queue.push_back((*counterparty_node_id, msg));
		}

		if let Some(peer_manager) = self.peer_manager.lock().unwrap().as_ref() {
			peer_manager.as_ref().process_events();
		}
	}
}
