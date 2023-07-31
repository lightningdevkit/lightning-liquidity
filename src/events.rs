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
//! [`crate::LiquidityManager::get_and_clear_pending_events()`] to receive events.

use crate::transport::msgs::{LSPS0Response, RequestId};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

#[derive(Default)]
pub(crate) struct EventQueue {
	queue: Mutex<VecDeque<Event>>,
	condvar: Condvar,
}

impl EventQueue {
	pub fn enqueue(&self, event: Event) {
		{
			let mut queue = self.queue.lock().unwrap();
			queue.push_back(event);
		}

		self.condvar.notify_one();
	}

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

/// Event which you should probably take some action in response to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
	/// The id from the request
	pub id: RequestId,
	/// The result of request
	pub result: EventResult,
}

/// Content of the event
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventResult {
	/// The LSPS0 response
	LSPS0(LSPS0Response),
}
