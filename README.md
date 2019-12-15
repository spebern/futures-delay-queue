# Asynchronous delay queue

<!-- cargo-sync-readme start -->

A queue of delayed elements running backed by [async-std](https://github.com/async-rs/async-std) and [futures-timer](https://github.com/async-rs/futures-timer).

Once an element is inserted into the `DelayQueue`, it is yielded once the
specified deadline has been reached.

The delayed items can be consumed through a channel returned at creation.

# Implementation

The delays are spawned and a timeout races against a reset channel that can be
triggered with the [`DelayHandle`]. If the timeout occurs before cancelation
or a reset the item is yielded through the receiver channel.

# Usage

Elements are inserted into `DelayQueue` using the [`insert`] or
[`insert_at`] methods. A deadline is provided with the item and a [`DelayHandle`] is
returned. The delay handle is used to remove the entry.

The delays can be configured with the [`reset_at` or the [`reset`] method or canceled by
calling the [`cancel`] method. Dropping the handle will not cancel the delay.

# Example

```rust
use futures_delay_queue::delay_queue;
use std::time::Duration;

#[async_std::main]
async fn main() {
    let (delay_queue, rx) = delay_queue::<i32>(3);

    let delay_handle = delay_queue.insert(1, Duration::from_millis(20));
    delay_handle.reset(Duration::from_millis(40)).await;
    let delay_handle = delay_queue.insert(2, Duration::from_millis(10));
    delay_handle.cancel().await;
    let delay_handle = delay_queue.insert(3, Duration::from_millis(30));

    assert_eq!(rx.recv().await, Some(3));
    assert_eq!(rx.recv().await, Some(1));

    drop(delay_queue);
    assert_eq!(rx.recv().await, None);
}
```

[`insert`]: #method.insert
[`insert_at`]: #method.insert_at
[`cancel`]: #method.cancel
[`DelayHandle`]: struct.DelayHandle.html

<!-- cargo-sync-readme end -->