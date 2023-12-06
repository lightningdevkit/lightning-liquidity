// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Events are surfaced by the library to indicate some action must be taken
//! by the end-user.
//!
//! Because we don't have a built-in runtime, it's up to the end-user to poll
//! [`LiquidityManager::get_and_clear_pending_events`] to receive events.
//!
//! [`LiquidityManager::get_and_clear_pending_events`]: crate::lsps0::message_handler::LiquidityManager::get_and_clear_pending_events

#[cfg(lsps1)]
use crate::lsps1;
use crate::lsps2;
use crate::prelude::{Vec, VecDeque};
use crate::sync::Mutex;

pub(crate) struct EventQueue {
	queue: Mutex<VecDeque<Event>>,
	#[cfg(feature = "std")]
	condvar: std::sync::Condvar,
}

impl EventQueue {
	pub fn new() -> Self {
		let queue = Mutex::new(VecDeque::new());
		#[cfg(feature = "std")]
		{
			let condvar = std::sync::Condvar::new();
			Self { queue, condvar }
		}
		#[cfg(not(feature = "std"))]
		Self { queue }
	}

	pub fn enqueue(&self, event: Event) {
		{
			let mut queue = self.queue.lock().unwrap();
			queue.push_back(event);
		}

		#[cfg(feature = "std")]
		self.condvar.notify_one();
	}

	pub fn next_event(&self) -> Option<Event> {
		self.queue.lock().unwrap().pop_front()
	}

	#[cfg(feature = "std")]
	pub fn wait_next_event(&self) -> Event {
		let mut queue =
			self.condvar.wait_while(self.queue.lock().unwrap(), |queue| queue.is_empty()).unwrap();

		let event = queue.pop_front().expect("non-empty queue");
		let should_notify = !queue.is_empty();

		drop(queue);

		if should_notify {
			self.condvar.notify_one();
		}

		event
	}

	pub fn get_and_clear_pending_events(&self) -> Vec<Event> {
		self.queue.lock().unwrap().drain(..).collect()
	}
}

/// An event which you should probably take some action in response to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
	/// An LSPS2 (JIT Channel) protocol event.
	LSPS2(lsps2::LSPS2Event),
	/// An LSPS1 protocol event.
	#[cfg(lsps1)]
	LSPS1(lsps1::event::Event),
}
