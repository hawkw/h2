use super::*;

#[derive(Debug)]
pub(super) struct Stream<B, P>
    where P: Peer,
{
    /// The h2 stream identifier
    pub id: StreamId,

    /// Current state of the stream
    pub state: State,

    // ===== Fields related to sending =====

    /// Next node in the accept linked list
    pub next_pending_send: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_send: bool,

    /// Send data flow control
    pub send_flow: FlowControl,

    /// Amount of send capacity that has been requested, but not yet allocated.
    pub requested_send_capacity: WindowSize,

    /// Amount of data buffered at the prioritization layer.
    /// TODO: Technically this could be greater than the window size...
    pub buffered_send_data: WindowSize,

    /// Task tracking additional send capacity (i.e. window updates).
    pub send_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<Frame<B>>,

    /// Next node in the linked list of streams waiting for additional
    /// connection level capacity.
    pub next_pending_send_capacity: Option<store::Key>,

    /// True if the stream is waiting for outbound connection capacity
    pub is_pending_send_capacity: bool,

    /// Set to true when the send capacity has been incremented
    pub send_capacity_inc: bool,

    // ===== Fields related to receiving =====

    /// Next node in the accept linked list
    pub next_pending_accept: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_accept: bool,

    /// Receive data flow control
    pub recv_flow: FlowControl,

    pub in_flight_recv_data: WindowSize,

    /// Next node in the linked list of streams waiting to send window updates.
    pub next_window_update: Option<store::Key>,

    /// True if the stream is waiting to send a window update
    pub is_pending_window_update: bool,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<recv::Event<P::Poll>>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// The stream's pending push promises
    pub pending_push_promises: store::Queue<B, NextAccept, P>,

    /// Validate content-length headers
    pub content_length: ContentLength,

}

/// State related to validating a stream's content-length
#[derive(Debug)]
pub enum ContentLength {
    Omitted,
    Head,
    Remaining(u64),
}

#[derive(Debug)]
pub(super) struct NextAccept;

#[derive(Debug)]
pub(super) struct NextSend;

#[derive(Debug)]
pub(super) struct NextSendCapacity;

#[derive(Debug)]
pub(super) struct NextWindowUpdate;

impl<B, P> Stream<B, P>
    where P: Peer,
{
    pub fn new(id: StreamId,
               init_send_window: WindowSize,
               init_recv_window: WindowSize) -> Stream<B, P>
    {
        let mut send_flow = FlowControl::new();
        let mut recv_flow = FlowControl::new();

        recv_flow.inc_window(init_recv_window)
            .ok().expect("invalid initial receive window");
        recv_flow.assign_capacity(init_recv_window);

        send_flow.inc_window(init_send_window)
            .ok().expect("invalid initial send window size");

        Stream {
            id,
            state: State::default(),

            // ===== Fields related to sending =====

            next_pending_send: None,
            is_pending_send: false,
            send_flow: send_flow,
            requested_send_capacity: 0,
            buffered_send_data: 0,
            send_task: None,
            pending_send: buffer::Deque::new(),
            is_pending_send_capacity: false,
            next_pending_send_capacity: None,
            send_capacity_inc: false,

            // ===== Fields related to receiving =====

            next_pending_accept: None,
            is_pending_accept: false,
            recv_flow: recv_flow,
            in_flight_recv_data: 0,
            next_window_update: None,
            is_pending_window_update: false,
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            pending_push_promises: store::Queue::new(),
            content_length: ContentLength::Omitted,
        }
    }

    pub fn assign_capacity(&mut self, capacity: WindowSize) {
        debug_assert!(capacity > 0);
        self.send_capacity_inc = true;
        self.send_flow.assign_capacity(capacity);

        // Only notify if the capacity exceeds the amount of buffered data
        if self.send_flow.available() > self.buffered_send_data {
            self.notify_send();
        }
    }

    /// Returns `Err` when the decrement cannot be completed due to overflow.
    pub fn dec_content_length(&mut self, len: usize) -> Result<(), ()> {
        match self.content_length {
            ContentLength::Remaining(ref mut rem) => {
                match rem.checked_sub(len as u64) {
                    Some(val) => *rem = val,
                    None => return Err(()),
                }
            }
            ContentLength::Head => return Err(()),
            _ => {}
        }

        Ok(())
    }

    pub fn ensure_content_length_zero(&self) -> Result<(), ()> {
        match self.content_length {
            ContentLength::Remaining(0) => Ok(()),
            ContentLength::Remaining(_) => Err(()),
            _ => Ok(()),
        }
    }

    pub fn notify_send(&mut self) {
        if let Some(task) = self.send_task.take() {
            task.notify();
        }
    }

    pub fn notify_recv(&mut self) {
        if let Some(task) = self.recv_task.take() {
            task.notify();
        }
    }
}

impl store::Next for NextAccept {
    fn next<B, P: Peer>(stream: &Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_accept
    }

    fn set_next<B, P: Peer>(stream: &mut Stream<B, P>, key: Option<store::Key>) {
        stream.next_pending_accept = key;
    }

    fn take_next<B, P: Peer>(stream: &mut Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_accept.take()
    }

    fn is_queued<B, P: Peer>(stream: &Stream<B, P>) -> bool {
        stream.is_pending_accept
    }

    fn set_queued<B, P: Peer>(stream: &mut Stream<B, P>, val: bool) {
        stream.is_pending_accept = val;
    }
}

impl store::Next for NextSend {
    fn next<B, P: Peer>(stream: &Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_send
    }

    fn set_next<B, P: Peer>(stream: &mut Stream<B, P>, key: Option<store::Key>) {
        stream.next_pending_send = key;
    }

    fn take_next<B, P: Peer>(stream: &mut Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_send.take()
    }

    fn is_queued<B, P: Peer>(stream: &Stream<B, P>) -> bool {
        stream.is_pending_send
    }

    fn set_queued<B, P: Peer>(stream: &mut Stream<B, P>, val: bool) {
        stream.is_pending_send = val;
    }
}

impl store::Next for NextSendCapacity {
    fn next<B, P: Peer>(stream: &Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_send_capacity
    }

    fn set_next<B, P: Peer>(stream: &mut Stream<B, P>, key: Option<store::Key>) {
        stream.next_pending_send_capacity = key;
    }

    fn take_next<B, P: Peer>(stream: &mut Stream<B, P>) -> Option<store::Key> {
        stream.next_pending_send_capacity.take()
    }

    fn is_queued<B, P: Peer>(stream: &Stream<B, P>) -> bool {
        stream.is_pending_send_capacity
    }

    fn set_queued<B, P: Peer>(stream: &mut Stream<B, P>, val: bool) {
        stream.is_pending_send_capacity = val;
    }
}

impl store::Next for NextWindowUpdate {
    fn next<B, P: Peer>(stream: &Stream<B, P>) -> Option<store::Key> {
        stream.next_window_update
    }

    fn set_next<B, P: Peer>(stream: &mut Stream<B, P>, key: Option<store::Key>) {
        stream.next_window_update = key;
    }

    fn take_next<B, P: Peer>(stream: &mut Stream<B, P>) -> Option<store::Key> {
        stream.next_window_update.take()
    }

    fn is_queued<B, P: Peer>(stream: &Stream<B, P>) -> bool {
        stream.is_pending_window_update
    }

    fn set_queued<B, P: Peer>(stream: &mut Stream<B, P>, val: bool) {
        stream.is_pending_window_update = val;
    }
}

// ===== impl ContentLength =====

impl ContentLength {
    pub fn is_head(&self) -> bool {
        match *self {
            ContentLength::Head => true,
            _ => false,
        }
    }
}
