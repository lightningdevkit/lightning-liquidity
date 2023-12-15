use crate::lsps0::msgs::LSPSMessage;
use crate::message_queue::MessageQueue;
use crate::prelude::{Vec, VecDeque};
use crate::sync::Mutex;

use lightning::sign::EntropySource;

use bitcoin::secp256k1::PublicKey;

pub(crate) struct TestMessageQueue {
	queue: Mutex<VecDeque<(PublicKey, LSPSMessage)>>,
}

impl TestMessageQueue {
	pub(crate) fn new() -> Self {
		let queue = Mutex::new(VecDeque::new());
		Self { queue }
	}

	pub(crate) fn get_and_clear_pending_msgs(&self) -> Vec<(PublicKey, LSPSMessage)> {
		self.queue.lock().unwrap().drain(..).collect()
	}
}

impl MessageQueue for TestMessageQueue {
	fn enqueue(&self, counterparty_node_id: &PublicKey, msg: LSPSMessage) {
		{
			let mut queue = self.queue.lock().unwrap();
			queue.push_back((*counterparty_node_id, msg));
		}
	}
}

pub struct TestEntropy {}
impl EntropySource for TestEntropy {
	fn get_secure_random_bytes(&self) -> [u8; 32] {
		[0; 32]
	}
}
