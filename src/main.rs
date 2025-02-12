use std::{
    collections::VecDeque,
    fmt::{write, Debug, Display},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread,
    time::Duration,
};

struct Executor {
    ready_queue: VecDeque<Arc<Task>>,
}
impl Executor {
    fn spawn(future: impl Future<Output = ()> + Send + 'static) {
        let future = Box::pin(future);
        let task = Arc::new(Task {
            future: Mutex::new(Some(future)),
        });
        unsafe { EXECUTOR.ready_queue.push_back(task) };
    }
    fn run(&mut self) {
        while let Some(task) = self.ready_queue.pop_front() {
            // Take the future, and if it has not yet completed (is still Some),
            // poll it in an attempt to complete it.
            let mut future_slot = task.future.lock().unwrap();
            if let Some(mut future) = future_slot.take() {
                // Create a `LocalWaker` from the task itself
                let waker = Waker::from(task.clone());
                let context = &mut Context::from_waker(&waker);
                // `BoxFuture<T>` is a type alias for
                // `Pin<Box<dyn Future<Output = T> + Send + 'static>>`.
                // We can get a `Pin<&mut dyn Future + Send + 'static>`
                // from it by calling the `Pin::as_mut` method.
                match future.as_mut().poll(context) {
                    Poll::Pending => {
                        // We're not done processing the future, so put it
                        // back in its task to be run again in the future.
                        *future_slot = Some(future);
                        context.waker().wake_by_ref();
                    }
                    Poll::Ready(v) => {}
                }
            }
        }
    }
}

static mut EXECUTOR: Executor = Executor {
    ready_queue: VecDeque::new(),
};

struct Task {
    /// In-progress future that should be pushed to completion.
    ///
    /// The `Mutex` is not necessary for correctness, since we only have
    /// one thread executing tasks at once. However, Rust isn't smart
    /// enough to know that `future` is only mutated from one thread,
    /// so we need to use the `Mutex` to prove thread-safety. A production
    /// executor would not need this, and could use `UnsafeCell` instead.
    future: Mutex<Option<Pin<Box<dyn Future<Output = ()> + Send>>>>,
}
impl Wake for Task {
    fn wake(self: Arc<Self>) {
        unsafe { EXECUTOR.ready_queue.push_back(self.clone()) };
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.clone().wake();
    }
}
impl Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "task")
    }
}

/* Sleep */
pub struct TimerFuture {
    shared_state: Arc<Mutex<SharedState>>,
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
impl TimerFuture {
    /// Create a new `TimerFuture` which will complete after the provided
    /// timeout.
    pub fn new(duration: Duration) -> Self {
        let shared_state = Arc::new(Mutex::new(SharedState {
            completed: false,
            waker: None,
        }));

        // Spawn the new thread
        let thread_shared_state = shared_state.clone();
        thread::spawn(move || {
            thread::sleep(duration);
            let mut shared_state = thread_shared_state.lock().unwrap();
            // Signal that the timer has completed and wake up the last
            // task on which the future was polled, if one exists.
            shared_state.completed = true;
            if let Some(waker) = shared_state.waker.take() {
                waker.wake()
            }
        });

        TimerFuture { shared_state }
    }
}
impl Future for TimerFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Look at the shared state to see if the timer has already completed.
        let mut shared_state = self.shared_state.lock().unwrap();
        if shared_state.completed {
            Poll::Ready(())
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

async fn hello(i: u64) {
    println!("Hello {i}!");
    world(i).await
}

async fn world(i: u64) {
    println!("waiting {i}");
    TimerFuture::new(Duration::from_secs(i)).await;
    println!("World {i}!");
}

fn main() {
    Executor::spawn(hello(10));
    Executor::spawn(hello(5));

    Executor::spawn(hello(2));

    Executor::spawn(hello(1));

    unsafe { EXECUTOR.run() };
}
