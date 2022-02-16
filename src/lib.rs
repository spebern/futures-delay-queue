//! A queue of delayed elements backed by [futures-timer](https://crates.io/crates/futures-timer)
//! that can be used with both:
//! - [async-std](https://crates.io/crates/async-std) as default, and
//! - [tokio](https://crates.io/crates/tokio) with feature "use-tokio"
//!
//! An element is inserted into the [`DelayQueue`] and will be yielded once the specified deadline
//! has been reached.
//!
//! The delayed items can be consumed through a channel returned at creation.
//!
//! # Implementation
//!
//! The delays are spawned and a timeout races against a reset channel that can be triggered with
//! the [`DelayHandle`]. If the timeout occurs before cancelation or a reset the item is yielded
//! through the receiver channel.
//!
//! # Usage
//!
//! A [`DelayQueue`] and a channel for receiving the expired items is created using the [`delay_queue`]
//! function.
//!
//! Elements are inserted into [`DelayQueue`] using the [`insert`] or [`insert_at`] methods. A
//! deadline is provided with the item and a [`DelayHandle`] is returned. The delay handle is used
//! to remove the entry.
//!
//! The delays can be configured with the [`reset_at`] or the [`reset`] method or canceled by
//! calling the [`cancel`] method. Dropping the handle will not cancel the delay.
//!
//! Modification of the delay fails if the delayed item expired in the meantime. In this case an
//! [`ErrorAlreadyExpired`] will be returned. If modification succeeds the handle will be returned
//! back to the caller.
//!
//! # Example
//!
//! ```rust
//! use futures_delay_queue::delay_queue;
//! use std::time::Duration;
//!
//! #[async_std::main]
//! async fn main() {
//!     let (delay_queue, rx) = delay_queue::<i32>();
//!
//!     let delay_handle = delay_queue.insert(1, Duration::from_millis(20));
//!     assert!(delay_handle.reset(Duration::from_millis(40)).await.is_ok());
//!
//!     let delay_handle = delay_queue.insert(2, Duration::from_millis(10));
//!     assert!(delay_handle.cancel().await.is_ok());
//!
//!     let delay_handle = delay_queue.insert(3, Duration::from_millis(30));
//!
//!     assert_eq!(rx.receive().await, Some(3));
//!     assert_eq!(rx.receive().await, Some(1));
//!
//!     drop(delay_queue);
//!     assert_eq!(rx.receive().await, None);
//! }
//! ```
//!
//! [`delay_queue`]: fn.delay_queue.html
//! [`DelayQueue`]: struct.DelayQueue.html
//! [`insert`]: struct.DelayQueue.html#method.insert
//! [`insert_at`]: struct.DelayQueue.html#method.insert_at
//! [`DelayHandle`]: struct.DelayHandle.html
//! [`cancel`]: struct.DelayHandle.html#method.cancel
//! [`reset`]: struct.DelayHandle.html#method.reset
//! [`reset_at`]: struct.DelayHandle.html#method.reset_at
//! [`ErrorAlreadyExpired`]: struct.ErrorAlreadyExpired.html

#![cfg_attr(feature = "docs", feature(doc_cfg))]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]
#![doc(test(attr(deny(rust_2018_idioms, warnings))))]
#![doc(test(attr(allow(unused_extern_crates, unused_variables))))]

#[cfg(feature = "use-async-std")]
use async_std::task;
use futures_intrusive::{
    buffer::{FixedHeapBuf, GrowingHeapBuf, RingBuf},
    channel::shared::{generic_channel, ChannelReceiveFuture, GenericReceiver},
};
use futures_timer::Delay;
use pin_project_lite::pin_project;
use std::{
    error::Error,
    fmt::{self, Display},
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
#[cfg(feature = "use-tokio")]
use tokio::task;

pub use futures_intrusive::channel::shared::{GenericSender, Receiver};

/// A queue for managing delayed items.
#[derive(Debug, Clone)]
pub struct DelayQueue<T: 'static, A: RingBuf<Item = T>> {
    // Used to send the expired items.
    expired: GenericSender<parking_lot::RawMutex, T, A>,
}

/// A handle to cancel the corresponding delay of an item in `DelayQueue`.
#[derive(Debug)]
pub struct DelayHandle {
    // Used to change the delay.
    reset: GenericSender<parking_lot::RawMutex, DelayReset, FixedHeapBuf<DelayReset>>,
}

enum DelayReset {
    NewDuration(Duration),
    Cancel,
}

/// The error type for delays that are modified after they have already expired in the `DelayQueue`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErrorAlreadyExpired {}

impl Error for ErrorAlreadyExpired {
    fn description(&self) -> &str {
        "delay already expired"
    }
}

impl Display for ErrorAlreadyExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Delay already expired")
    }
}

impl DelayHandle {
    /// Resets the delay of the corresponding item to `when` and returns a new `DelayHandle` on
    /// success.
    pub async fn reset_at(self, when: Instant) -> Result<Self, ErrorAlreadyExpired> {
        let now = Instant::now();
        let dur = if when <= now {
            Duration::from_nanos(0)
        } else {
            when - now
        };
        self.reset(dur).await
    }

    /// Resets the delay of the corresponding item to now + `dur` and returns a new `DelayHandle`on
    /// success.
    pub async fn reset(self, dur: Duration) -> Result<Self, ErrorAlreadyExpired> {
        self.reset
            .send(DelayReset::NewDuration(dur))
            .await
            .map_err(|_| ErrorAlreadyExpired {})?;
        Ok(self)
    }

    /// Cancels the delay.
    pub async fn cancel(self) -> Result<(), ErrorAlreadyExpired> {
        self.reset
            .send(DelayReset::Cancel)
            .await
            .map_err(|_| ErrorAlreadyExpired {})
    }
}

/// Creates a dynamically growing delay queue and a multi consumer channel for receiving expired items.
///
/// # Example
///
/// ```
/// # async_std::task::block_on(async {
/// #
/// use futures_delay_queue::delay_queue;
/// use std::time::Duration;
///
/// let (delay_queue, expired_items) = delay_queue();
/// delay_queue.insert(1, Duration::from_millis(10));
///
/// // approximately 10ms later
/// assert_eq!(expired_items.receive().await, Some(1));
/// #
/// # })
/// ```
pub fn delay_queue<T: 'static + Send>() -> (DelayQueue<T, GrowingHeapBuf<T>>, GenericReceiver<parking_lot::RawMutex, T, GrowingHeapBuf<T>>) {
    let (tx, rx) = generic_channel(0);
    (DelayQueue { expired: tx }, rx)
}

pin_project! {
    struct DelayedItem<T> {
        value: Option<T>,
        delay: Delay,
        reset_rx: GenericReceiver<parking_lot::RawMutex, DelayReset, FixedHeapBuf<DelayReset>>,
        reset: ChannelReceiveFuture<parking_lot::RawMutex, DelayReset>,
        handle_dropped: bool,
    }
}

impl<T> Future for DelayedItem<T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.handle_dropped {
            // Make sure the reset future is polled at least once to be awaken on new messages.
            loop {
                // check if we got a reset or got cancled
                // `new_unchecked` is ok because the value is never used again after being dropped.
                if let Poll::Ready(v) = unsafe { Pin::new_unchecked(&mut self.reset).poll(cx) } {
                    match v {
                        Some(reset) => match reset {
                            DelayReset::Cancel => return Poll::Ready(None),
                            DelayReset::NewDuration(dur) => self.delay = Delay::new(dur),
                        },
                        // handle got dropped, from now on we wait until the item expires
                        None => {
                            self.handle_dropped = true;
                            // Channel is closed.
                            break;
                        }
                    }
                    // Future is consumed, ask for a new one and poll it (loop).
                    self.reset = self.reset_rx.receive();
                } else {
                    break;
                }
            }
        }

        // expired?
        match Pin::new(&mut self.delay).poll(cx) {
            Poll::Ready(_) => Poll::Ready(self.value.take()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T, A> DelayQueue<T, A>
where
    T: 'static + Send,
    A: 'static + RingBuf<Item = T> + Send,
{
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
        let (reset_tx, reset_rx) = generic_channel::<parking_lot::RawMutex, _, FixedHeapBuf<_>>(0);
        let expired = self.expired.clone();
        let reset = reset_rx.receive();
        let delayed_item = DelayedItem {
            value: Some(value),
            delay: Delay::new(dur),
            reset_rx,
            reset,
            handle_dropped: false,
        };
        task::spawn(async move {
            if let Some(v) = delayed_item.await {
                let _ = expired.send(v).await;
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
        let (delay_queue, rx) = delay_queue::<i32>();
        delay_queue.insert(1, Duration::from_millis(10));
        delay_queue.insert(2, Duration::from_millis(5));
        assert_eq!(
            timeout(Duration::from_millis(8), rx.receive()).await,
            Ok(Some(2))
        );
        assert_eq!(
            timeout(Duration::from_millis(7), rx.receive()).await,
            Ok(Some(1))
        );
    }

    #[async_std::test]
    async fn reset() {
        let (delay_queue, rx) = delay_queue::<i32>();
        let delay_handle = delay_queue.insert(1, Duration::from_millis(100));
        assert!(delay_handle.reset(Duration::from_millis(20)).await.is_ok());

        assert_eq!(
            timeout(Duration::from_millis(40), rx.receive()).await,
            Ok(Some(1))
        );

        let delay_handle = delay_queue.insert(2, Duration::from_millis(100));
        assert!(
            delay_handle
                .reset_at(Instant::now() + Duration::from_millis(20))
                .await
                .is_ok()
        );

        assert_eq!(
            timeout(Duration::from_millis(40), rx.receive()).await,
            Ok(Some(2))
        );
    }

    #[async_std::test]
    async fn cancel() {
        let (delay_queue, rx) = delay_queue::<i32>();
        let delay_handle = delay_queue.insert(1, Duration::from_millis(200));
        // Make sure the future is polled at least once
        task::sleep(Duration::from_millis(50)).await;
        let instant = Instant::now();
        assert!(delay_handle.cancel().await.is_ok());
        assert!(instant.elapsed() < Duration::from_millis(10));
        assert!(
            timeout(Duration::from_millis(500), rx.receive())
                .await
                .is_err()
        );
    }
}
