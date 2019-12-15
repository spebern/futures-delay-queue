//! A queue of delayed elements running backed by [async-std](https://github.com/async-rs/async-std) and [futures-timer](https://github.com/async-rs/futures-timer).
//!
//! Once an element is inserted into the `DelayQueue`, it is yielded once the
//! specified deadline has been reached.
//!
//! The delayed items can be consumed through a channel returned at creation.
//!
//! # Implementation
//!
//! The delays are spawned and a timeout races against a reset channel that can be
//! triggered with the [`DelayHandle`]. If the timeout occurs before cancelation
//! or a reset the item is yielded through the receiver channel.
//!
//! # Usage
//!
//! Elements are inserted into `DelayQueue` using the [`insert`] or
//! [`insert_at`] methods. A deadline is provided with the item and a [`DelayHandle`] is
//! returned. The delay handle is used to remove the entry.
//!
//! The delays can be configured with the [`reset_at` or the [`reset`] method or canceled by
//! calling the [`cancel`] method. Dropping the handle will not cancel the delay.
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
//!     let d1 = dq.insert(1, Duration::from_secs(2));
//!     d1.reset(Duration::from_secs(4)).await;
//!     let d2 = dq.insert(2, Duration::from_secs(1));
//!     d2.cancel().await;
//!     let d3 = dq.insert(3, Duration::from_secs(3));
//!
//!     assert_eq!(rx.recv().await, Some(3));
//!     assert_eq!(rx.recv().await, Some(1));
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
    stream::Stream,
    sync::{channel, Receiver, Sender},
    task,
};
use futures_timer::Delay;
use pin_project_lite::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

/// A queue for managing delayed items.
#[derive(Debug)]
pub struct DelayQueue<T: 'static> {
    // Used to send the expired items.
    expired: Sender<T>,
}

/// A handle to cancel the corresponding delay of an item in `DelayQueue`.
#[derive(Debug)]
pub struct DelayHandle {
    // Used to change the delay.
    reset: Sender<DelayReset>,
}

enum DelayReset {
    NewDuration(Duration),
    Cancel,
}

impl DelayHandle {
    /// Resets the delay of the corresponding item to `when` and returns a new `DelayHandle` on
    /// success.
    pub async fn reset_at(&self, when: Instant) {
        let now = Instant::now();
        let dur = if when <= now {
            Duration::from_nanos(0)
        } else {
            when - now
        };
        self.reset(dur).await;
    }

    /// Resets the delay of the corresponding item to now + `dur` and returns a new `DelayHandle`on
    /// success.
    pub async fn reset(&self, dur: Duration) {
        self.reset.send(DelayReset::NewDuration(dur)).await;
    }

    /// Cancels the delay.
    pub async fn cancel(self) {
        self.reset.send(DelayReset::Cancel).await;
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

pin_project! {
    struct DelayedItem<T> {
        value: Option<T>,
        #[pin]
        delay: Delay,
        #[pin]
        reset: Receiver<DelayReset>,
        handle_dropped: bool,
    }
}

impl<T> Future for DelayedItem<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if !*this.handle_dropped {
            // check if we got a reset or got cancled
            if let Poll::Ready(v) = this.reset.poll_next(cx) {
                match v {
                    Some(reset) => match reset {
                        DelayReset::Cancel => return Poll::Ready(None),
                        DelayReset::NewDuration(dur) => *this.delay = Delay::new(dur),
                    },
                    // cancel
                    None => *this.handle_dropped = true,
                }
            }
        }

        // expired?
        match this.delay.poll(cx) {
            Poll::Ready(_) => Poll::Ready(this.value.take()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: 'static + Send> DelayQueue<T> {
    /// Inserts an item into the delay queue that will be yielded after `dur` has passed.
    pub fn insert(&self, value: T, dur: Duration) -> DelayHandle {
        self.new_handle_with_future(value, dur)
    }

    /// Inserts an item into the delay queue that will be yielded at `when`.
    pub fn insert_at(&self, value: T, when: Instant) -> DelayHandle {
        let now = Instant::now();
        let dur = if now >= when {
            Duration::from_nanos(0)
        } else {
            when - now
        };
        self.new_handle_with_future(value, dur)
    }

    fn new_handle_with_future(&self, value: T, dur: Duration) -> DelayHandle {
        let (reset_tx, reset_rx) = channel(1);
        let expired = self.expired.clone();
        let delayed_item = DelayedItem {
            value: Some(value),
            delay: Delay::new(dur),
            reset: reset_rx,
            handle_dropped: false,
        };
        task::spawn(async move {
            if let Some(v) = delayed_item.await {
                expired.send(v).await;
            }
        });
        DelayHandle { reset: reset_tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_std::future::timeout;

    #[async_std::test]
    async fn insert() {
        let (delay_queue, rx) = delay_queue::<i32>(3);
        delay_queue.insert(1, Duration::from_millis(10));
        delay_queue.insert(2, Duration::from_millis(5));
        assert_eq!(
            timeout(Duration::from_millis(8), rx.recv()).await,
            Ok(Some(2))
        );
        assert_eq!(
            timeout(Duration::from_millis(7), rx.recv()).await,
            Ok(Some(1))
        );
    }

    #[async_std::test]
    async fn reset() {
        let (delay_queue, rx) = delay_queue::<i32>(3);
        let delay_handle = delay_queue.insert(1, Duration::from_millis(100));
        delay_handle.reset(Duration::from_millis(20)).await;
        assert_eq!(
            timeout(Duration::from_millis(40), rx.recv()).await,
            Ok(Some(1))
        );

        let delay_handle = delay_queue.insert(2, Duration::from_millis(100));
        delay_handle.reset_at(Instant::now() + Duration::from_millis(20)).await;
        assert_eq!(
            timeout(Duration::from_millis(40), rx.recv()).await,
            Ok(Some(2))
        );
    }

    #[async_std::test]
    async fn cancel() {
        let (dq, rx) = delay_queue::<i32>(3);
        let delay_handle = dq.insert(1, Duration::from_millis(20));
        delay_handle.cancel().await;
        assert!(timeout(Duration::from_millis(40), rx.recv()).await.is_err());
    }
}

