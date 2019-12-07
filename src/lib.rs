//! A queue of delayed elements running backed by [async-std](https://github.com/async-rs/async-std) and [futures-timer](https://github.com/async-rs/futures-timer).
//!
//! Once an element is inserted into the `DelayQueue`, it is yielded once the
//! specified deadline has been reached.
//!
//! The delayed items can be consumed through a channel returned at creation.
//!
//! # Implementation
//!
//! The delays are spawned and a timeout races against a oneshot channel that can be
//! triggered with the [`DelayHandle`]. If the timeout occurs before cancelation the
//! item is yielded through the receiver channel.
//!
//! # Usage
//!
//! Elements are inserted into `DelayQueue` using the [`insert`] or
//! [`insert_at`] methods. A deadline is provided with the item and a [`DelayHandle`] is
//! returned. The delay handle is used to remove the entry.
//!
//! The delays can be canceled by calling the [`cancel`] method on the [`DelayHandle`].
//!
//! # Example
//!
//! ```
//! use futures_delay_queue::delay_queue;
//! use std::time::Duration;
//!
//! #[async_std::main]
//! async fn main() {
//!     let (dq, rx) = delay_queue::<i32>(3);
//!
//!     let _ = dq.insert(1, Duration::from_secs(2));
//!     let d = dq.insert(2, Duration::from_secs(1));
//!     d.cancel();
//!     let _ = dq.insert(3, Duration::from_secs(3));
//!
//!     assert_eq!(rx.recv().await, Some(1));
//!     assert_eq!(rx.recv().await, Some(3));
//!
//!     drop(dq);
//!     assert_eq!(rx.recv().await, None);
//! }
//! ```
//!
//! [`insert`]: #method.insert
//! [`insert_at`]: #method.insert_at
//! [`cancel`]: #method.cancel
//! [`DelayHandle`]: struct.DelayHandle.html

#![cfg_attr(feature = "docs", feature(doc_cfg))]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]
#![doc(test(attr(deny(rust_2018_idioms, warnings))))]
#![doc(test(attr(allow(unused_extern_crates, unused_variables))))]

use async_std::{
    future::{self, TimeoutError},
    sync::{channel, Arc, Receiver, Sender},
    task,
};
use futures_intrusive::channel::OneshotChannel;
use std::time::{Duration, Instant};

/// A queue for managing delayed items.
#[derive(Debug)]
pub struct DelayQueue<T> {
    // Used to send the expired items.
    expired: Sender<T>,
}

/// A handle to cancel the corresponding delay of an item in `DelayQueue`.
#[derive(Debug)]
pub struct DelayHandle {
    /// Used to cancel the delay.
    cancel: Arc<OneshotChannel<()>>,
}

impl DelayHandle {
    /// Cancels the delay of the corresponding delayed item.
    pub fn cancel(self) {
        // Send error can only occur if the channel was closed, which happens
        // when the item already expired.
        let _ = self.cancel.send(());
    }
}

/// Creates a delay queue and a multi consumer channel for receiving expired items.
///
/// This delay queue has a buffer that can hold at most `cap` messages at a time.
///
/// # Panics
///
/// If `cap` is zero, this function will panic.
pub fn delay_queue<T: 'static + Send>(cap: usize) -> (DelayQueue<T>, Receiver<T>) {
    let (tx, rx) = channel(cap);
    (DelayQueue { expired: tx }, rx)
}

impl<T: 'static + Send> DelayQueue<T> {
    /// Inserts an item into the delay queue that will be yielded after `timeout` has passed.
    pub fn insert(&self, value: T, timeout: Duration) -> DelayHandle {
        self.new_handle_with_future(value, timeout)
    }

    /// Inserts an item into the delay queue that will be yielded at `when`.
    pub fn insert_at(&self, value: T, when: Instant) -> DelayHandle {
        let now = Instant::now();
        let timeout = if now >= when {
            Duration::from_nanos(0)
        } else {
            when - now
        };
        self.new_handle_with_future(value, timeout)
    }

    fn new_handle_with_future(&self, value: T, timeout: Duration) -> DelayHandle {
        let delay_handle = DelayHandle {
            cancel: Arc::new(OneshotChannel::new()),
        };
        let cancel = delay_handle.cancel.clone();
        let expired = self.expired.clone();
        task::spawn(async move {
            if let Err(TimeoutError { .. }) = future::timeout(timeout, cancel.receive()).await {
                expired.send(value).await;
            }
        });
        delay_handle
    }
}
