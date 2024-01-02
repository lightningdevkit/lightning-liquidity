//! Holds types and traits used to implement message queues for [`LSPSMessage`]s.

use crate::lsps0::msgs::LSPSMessage;
use crate::prelude::{Box, Vec, VecDeque};
use crate::sync::{Mutex, RwLock};

use bitcoin::secp256k1::PublicKey;

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
pub struct DefaultMessageQueue {
	queue: Mutex<VecDeque<(PublicKey, LSPSMessage)>>,
	process_msgs_callback: RwLock<Option<Box<dyn Fn() + Send + Sync + 'static>>>,
}

impl DefaultMessageQueue {
	pub(crate) fn new() -> Self {
		let queue = Mutex::new(VecDeque::new());
		let process_msgs_callback = RwLock::new(None);
		Self { queue, process_msgs_callback }
	}

	pub(crate) fn set_process_msgs_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
		*self.process_msgs_callback.write().unwrap() = Some(Box::new(callback));
	}

	pub(crate) fn get_and_clear_pending_msgs(&self) -> Vec<(PublicKey, LSPSMessage)> {
		self.queue.lock().unwrap().drain(..).collect()
	}
}

impl MessageQueue for DefaultMessageQueue {
	fn enqueue(&self, counterparty_node_id: &PublicKey, msg: LSPSMessage) {
		{
			let mut queue = self.queue.lock().unwrap();
			queue.push_back((*counterparty_node_id, msg));
		}

		if let Some(process_msgs_callback) = self.process_msgs_callback.read().unwrap().as_ref() {
			(process_msgs_callback)()
		}
	}
}
