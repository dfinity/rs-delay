#![cfg(not(feature = "no_std"))]
use crate::{Waiter, WaiterError};
use std::cell::RefCell;

#[cfg(feature = "async")]
use core::{future::Future, pin::Pin};
use std::thread::sleep;
use std::time::Duration;

#[cfg(feature = "async")]
mod future {
    use crate::WaiterError;
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use core::thread::{sleep, spawn};
    use core::time::Duration;

    /// A Future that resolves when a time has passed.
    /// This is based on [https://rust-lang.github.io/async-book/02_execution/03_wakeups.html].
    pub(super) struct ThrottleTimerFuture {
        shared_state: SharedState,
    }

    /// Shared state between the future and the waiting thread
    struct SharedState {
        /// Whether or not the sleep time has elapsed
        completed: bool,

        /// The waker for the task that `TimerFuture` is running on.
        /// The thread can use this after setting `completed = true` to tell
        /// `TimerFuture`'s task to wake up, see that `completed = true`, and
        /// move forward.
        waker: Option<Waker>,
    }

    impl Future for ThrottleTimerFuture {
        type Output = Result<(), WaiterError>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // Look at the shared state to see if the timer has already completed.
            let mut shared_state = self.shared_state.lock().unwrap();
            if shared_state.completed {
                Poll::Ready(Ok(()))
            } else {
                // Set waker so that the thread can wake up the current task
                // when the timer has completed, ensuring that the future is polled
                // again and sees that `completed = true`.
                //
                // It's tempting to do this once rather than repeatedly cloning
                // the waker each time. However, the `TimerFuture` can move between
                // tasks on the executor, which could cause a stale waker pointing
                // to the wrong task, preventing `TimerFuture` from waking up
                // correctly.
                //
                // N.B. it's possible to check for this using the `Waker::will_wake`
                // function, but we omit that here to keep things simple.
                shared_state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    impl ThrottleTimerFuture {
        /// Create a new `TimerFuture` which will complete after the provided
        /// timeout.
        pub fn new(duration: Duration) -> Self {
            let shared_state = SharedState {
                completed: false,
                waker: None,
            };

            // Spawn the new thread
            let thread_shared_state = shared_state.clone();
            spawn(move || {
                sleep(duration);
                let mut shared_state = thread_shared_state.lock().unwrap();
                // Signal that the timer has completed and wake up the last
                // task on which the future was polled, if one exists.
                shared_state.completed = true;
                if let Some(waker) = shared_state.waker.take() {
                    waker.wake()
                }
            });

            ThrottleTimerFuture { shared_state }
        }
    }
}

#[derive(Clone)]
pub struct ThrottleWaiter {
    throttle: Duration,
}
impl ThrottleWaiter {
    pub fn new(throttle: Duration) -> Self {
        Self { throttle }
    }
}
impl Waiter for ThrottleWaiter {
    fn wait(&self) -> Result<(), WaiterError> {
        sleep(self.throttle);

        Ok(())
    }

    #[cfg(feature = "async")]
    fn async_wait(&self) -> Pin<Box<dyn Future<Output = Result<(), WaiterError>>>> {
        Box::pin(future::ThrottleTimerFuture::new(self.throttle))
    }
}

#[derive(Clone)]
pub struct ExponentialBackoffWaiter {
    next: Option<RefCell<Duration>>,
    initial: Duration,
    multiplier: f32,
    cap: Duration,
}
impl ExponentialBackoffWaiter {
    pub fn new(initial: Duration, multiplier: f32, cap: Duration) -> Self {
        ExponentialBackoffWaiter {
            next: None,
            initial,
            multiplier,
            cap,
        }
    }
}
impl Waiter for ExponentialBackoffWaiter {
    fn restart(&mut self) -> Result<(), WaiterError> {
        let next = self.next.as_ref().ok_or(WaiterError::NotStarted)?;
        next.replace(self.initial);
        Ok(())
    }

    fn start(&mut self) {
        self.next = Some(RefCell::new(self.initial));
    }

    fn wait(&self) -> Result<(), WaiterError> {
        let next = self.next.as_ref().ok_or(WaiterError::NotStarted)?;
        let current = *next.borrow();
        let current_nsec = current.as_nanos() as f32;

        // Find the next throttle.
        let mut next_duration = Duration::from_nanos((current_nsec * self.multiplier) as u64);
        if next_duration > self.cap {
            next_duration = self.cap;
        }

        next.replace(next_duration);

        std::thread::sleep(current);

        Ok(())
    }

    #[cfg(feature = "async")]
    fn async_wait(&self) -> Pin<Box<dyn Future<Output = Result<(), WaiterError>>>> {
        let next = if let Some(next) = self.next.as_ref() {
            next
        } else {
            return Box::pin(std::future::ready(Err(WaiterError::NotStarted)));
        };

        let current = *next.borrow();
        let current_nsec = current.as_nanos() as f32;

        // Find the next throttle.
        let mut next_duration = Duration::from_nanos((current_nsec * self.multiplier) as u64);
        if next_duration > self.cap {
            next_duration = self.cap;
        }

        next.replace(next_duration);

        Box::pin(future::ThrottleTimerFuture::new(current))
    }
}
