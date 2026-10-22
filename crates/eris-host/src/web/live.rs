//! The turns this process is running, by conversation.
//!
//! A turn writes to the page that asked for it through the response it already
//! holds. That works until the page goes away: an iPad that suspends Safari
//! mid-answer drops the socket, and the turn carries on writing into a channel
//! nobody is reading. Reloading then showed a row still marked `streaming` and
//! nothing else - the transcript said "Streaming" for as long as the turn ran,
//! and the only way to see what it had done was to keep reloading until it
//! stopped.
//!
//! This is the other end of that socket. Every turn registers here for as long
//! as it runs, publishes the same frames it sends to the page that asked, and
//! unregisters when the task ends. Two questions are then answerable from one
//! table: "what is running in this conversation", for a page that arrives late;
//! and "is anything actually running", for a row that claims a turn is writing
//! it. Both are answered from process state rather than from a column that
//! something forgot to clear, which is what [`super::chat::fail_orphaned`]
//! cannot do for a turn that died without a restart.
//!
//! Nothing here is durable, on purpose. A turn is a running task and its record
//! is the row; a process restart ends every turn, which is exactly the reading
//! `fail_orphaned` already takes at boot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One frame, as the page receives it.
///
/// The event name is `&'static str` because the set is closed: a subscriber that
/// had to parse a name it had never heard of would be a subscriber guessing.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The SSE event name.
    pub event: &'static str,
    /// The event's data, already shaped for the page.
    pub data: serde_json::Value,
    /// Where this frame sits in its turn's sequence, from 1.
    ///
    /// Sent to the page as the SSE `id:` line, which is what the browser echoes
    /// back as `Last-Event-ID` when it reconnects on its own. Without it a page
    /// whose socket dropped silently resumed at whatever frame came next, and
    /// the text that arrived while it was away was never seen: the answer on
    /// screen was short by exactly the frames it missed, and nothing said so.
    pub seq: u64,
}

/// How many frames a subscriber may fall behind before it is told to resync,
/// and how many of a running turn's most recent frames are kept to replay.
///
/// Bounded so a page that stops reading cannot grow the daemon's memory, and so
/// a turn that runs for half an hour cannot either. A subscriber that lags past
/// the ring is not quietly missing frames: the stream says `resync` and the page
/// asks for the row again, which is the same state a reload has.
const BACKLOG: usize = 256;

/// Every turn this process is running, by conversation.
///
/// Cloned rather than borrowed: it is installed in the handler state and the
/// turn task both, and both are `'static`.
#[derive(Clone, Default)]
pub struct LiveHub {
    inner: Arc<Mutex<HashMap<String, Run>>>,
}

/// What one running turn has published, and to whom.
struct Run {
    /// The answer row this turn is writing.
    answer: String,
    /// Every page watching it. A `Sender` with no receivers still succeeds, which
    /// is what makes a turn with no page reading it cost nothing.
    frames: tokio::sync::broadcast::Sender<Arc<Frame>>,
    /// The most recent frames, oldest first, for a page that reconnects and says
    /// where it got to. Kept in the lock the table already holds, so the ring and
    /// the registration cannot disagree about what a turn published.
    history: std::collections::VecDeque<Arc<Frame>>,
}

impl std::fmt::Debug for LiveHub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveHub")
            .field("running", &self.len())
            .finish()
    }
}

impl LiveHub {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many conversations have a turn running in this process.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is running.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Register a turn, and hand back the handle it publishes through.
    ///
    /// The handle is the registration: dropping it unregisters the turn, so a
    /// task that returns, panics or is dropped cannot leave a row looking live.
    /// A second turn in the same conversation replaces the first, because one
    /// conversation runs one turn and the newer answer is the one that exists.
    pub fn begin(&self, conversation: &str, answer: &str) -> LiveTurn {
        let (frames, _) = tokio::sync::broadcast::channel(BACKLOG);
        let handle = LiveTurn {
            hub: self.clone(),
            conversation: conversation.to_owned(),
            answer: answer.to_owned(),
            frames,
        };
        self.lock().insert(
            conversation.to_owned(),
            Run {
                answer: answer.to_owned(),
                frames: handle.frames.clone(),
                history: std::collections::VecDeque::new(),
            },
        );
        handle
    }

    /// Whether this process is running a turn in that conversation.
    ///
    /// The answer row's id, when it is: a page that attaches can then say which
    /// row it is watching rather than assuming the newest one.
    #[must_use]
    pub fn running(&self, conversation: &str) -> Option<String> {
        self.lock().get(conversation).map(|run| run.answer.clone())
    }

    /// Watch a conversation, or `None` when nothing is running in it.
    ///
    /// Subscribing happens under the same lock as the lookup, so a turn that
    /// ends between the two cannot be reported as running and then hand back a
    /// receiver for a channel nobody will ever publish to.
    #[must_use]
    pub fn watch(&self, conversation: &str) -> Option<Watch> {
        self.watch_after(conversation, None)
    }

    /// Watch a conversation, replaying the frames after `after` when asked.
    ///
    /// `after` is the sequence the page last saw, from the `Last-Event-ID` the
    /// browser sends when it reconnects by itself. Frames the page missed are
    /// handed back in the same call that subscribes, so a frame published
    /// between the two cannot be lost in the gap or delivered twice.
    ///
    /// A page that has fallen further behind than the ring holds gets
    /// `missed: true` instead of a partial replay: its own text is short by an
    /// unknown amount, and a page that cannot be caught up has to be repainted
    /// from its snapshot rather than patched.
    #[must_use]
    pub fn watch_after(&self, conversation: &str, after: Option<u64>) -> Option<Watch> {
        let guard = self.lock();
        let run = guard.get(conversation)?;
        let frames = run.frames.subscribe();
        let mut replay = Vec::new();
        let mut missed = false;
        if let Some(after) = after {
            let oldest = run.history.front().map(|frame| frame.seq).unwrap_or(0);
            // A gap in the ring, or a page that never saw this turn at all.
            missed = run
                .history
                .front()
                .is_some_and(|_| oldest > after.saturating_add(1));
            if !missed {
                replay = run
                    .history
                    .iter()
                    .filter(|frame| frame.seq > after)
                    .cloned()
                    .collect();
            }
        }
        Some(Watch {
            answer: run.answer.clone(),
            frames,
            replay,
            missed,
        })
    }

    /// Publish one frame on behalf of a running turn.
    ///
    /// The sequence is assigned here rather than by the caller, because the
    /// numbers have to be dense and in order and the callers are a turn task, a
    /// subagent callback and occasionally both at once. The last [`BACKLOG`]
    /// frames are kept for a page that reconnects and names the one it saw.
    fn publish_frame(&self, conversation: &str, mut frame: Frame) -> Arc<Frame> {
        let mut guard = self.lock();
        let Some(run) = guard.get_mut(conversation) else {
            // The turn retired between the caller reading the table and here. The
            // frame goes out to whoever is still reading the broadcast - a page
            // that is mid-read is better served by the frame than by silence -
            // and there is nothing to number it against.
            frame.seq = 0;
            return Arc::new(frame);
        };
        let seq = run.history.back().map_or(1, |last| last.seq + 1);
        frame.seq = seq;
        let frame = Arc::new(frame);
        run.history.push_back(frame.clone());
        while run.history.len() > BACKLOG {
            let _ = run.history.pop_front();
        }
        let _ = run.frames.send(frame.clone());
        frame
    }

    /// Take a finished turn out of the table.
    ///
    /// Called by [`LiveTurn`]'s drop, and only for a turn that is still the one
    /// registered: a task that ends after a newer turn started in the same
    /// conversation must not unregister the newer one.
    fn retire(&self, conversation: &str, answer: &str) {
        let mut guard = self.lock();
        if guard
            .get(conversation)
            .is_some_and(|run| run.answer == answer)
        {
            guard.remove(conversation);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Run>> {
        // Poisoned means a panic happened while the table was held. The table is
        // a map of channels and holds no invariant a panic could have broken, so
        // the guard is taken back rather than propagated: a page that cannot see
        // a running turn is worse than a stale entry.
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// One running turn's end of the table.
///
/// Held by the task that runs the turn. Publishing to it costs one lock and one
/// send; a conversation with no watcher still pays for the send, which is a
/// `broadcast` with no receivers and returns immediately.
pub struct LiveTurn {
    hub: LiveHub,
    conversation: String,
    answer: String,
    frames: tokio::sync::broadcast::Sender<Arc<Frame>>,
}

/// A publisher that does not own the registration.
///
/// Cloneable and `'static`, because some of a turn's frames are sent from a
/// callback that outlives the borrow of the turn - a subagent reporting its
/// status, for one. It cannot unregister anything: only dropping the
/// [`LiveTurn`] does that, so a detached publisher can never make a running turn
/// look finished.
#[derive(Clone)]
pub struct Reporter {
    hub: LiveHub,
    conversation: String,
}

impl Reporter {
    /// Send one frame to every page watching this conversation.
    pub fn publish(&self, event: &'static str, data: serde_json::Value) {
        let _ = self.hub.publish_frame(
            &self.conversation,
            Frame {
                event,
                data,
                seq: 0,
            },
        );
    }
}

impl std::fmt::Debug for LiveTurn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveTurn")
            .field("conversation", &self.conversation)
            .field("answer", &self.answer)
            .field("watchers", &self.frames.receiver_count())
            .finish()
    }
}

impl LiveTurn {
    /// The answer row this turn is writing.
    #[must_use]
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// Send one frame to every page watching this conversation.
    ///
    /// The error a closed channel returns is dropped: a turn that published its
    /// last frame to nobody has still run, and the row is the record.
    pub fn publish(&self, event: &'static str, data: serde_json::Value) {
        let _ = self.hub.publish_frame(
            &self.conversation,
            Frame {
                event,
                data,
                seq: 0,
            },
        );
    }

    /// A publisher that can be handed to a callback without lending it the turn.
    #[must_use]
    pub fn reporter(&self) -> Reporter {
        Reporter {
            hub: self.hub.clone(),
            conversation: self.conversation.clone(),
        }
    }

    /// Whether anybody is watching.
    ///
    /// Read by nothing that changes behaviour, and kept because the honest
    /// reading of a stream with no reader is worth being able to ask.
    #[must_use]
    pub fn watched(&self) -> bool {
        self.frames.receiver_count() > 0
    }
}

impl Drop for LiveTurn {
    fn drop(&mut self) {
        self.hub.retire(&self.conversation, &self.answer);
    }
}

/// One page's view of a running turn.
pub struct Watch {
    /// The answer row the turn is writing.
    pub answer: String,
    /// The frames, from the point of subscribing.
    pub frames: tokio::sync::broadcast::Receiver<Arc<Frame>>,
    /// Frames the page missed, oldest first, when it said where it got to.
    pub replay: Vec<Arc<Frame>>,
    /// Whether the page is too far behind for the ring to catch it up.
    pub missed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_with_no_turn_reports_nothing_running() {
        let hub = LiveHub::new();
        assert_eq!(hub.running("c1"), None);
        assert!(hub.watch("c1").is_none());
        assert!(hub.is_empty());
    }

    #[test]
    fn a_running_turn_is_visible_and_names_its_answer_row() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        assert_eq!(hub.running("c1").as_deref(), Some("answer-1"));
        assert_eq!(turn.answer(), "answer-1");
        assert_eq!(
            hub.running("other"),
            None,
            "another conversation runs nothing"
        );
    }

    #[test]
    fn dropping_the_turn_unregisters_it() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        assert!(hub.running("c1").is_some());
        drop(turn);
        assert_eq!(
            hub.running("c1"),
            None,
            "a task that returned must not leave the row looking live"
        );
    }

    #[test]
    fn an_older_turn_cannot_unregister_a_newer_one() {
        let hub = LiveHub::new();
        let first = hub.begin("c1", "answer-1");
        let second = hub.begin("c1", "answer-2");
        drop(first);
        assert_eq!(
            hub.running("c1").as_deref(),
            Some("answer-2"),
            "the newer turn owns the conversation"
        );
        drop(second);
        assert_eq!(hub.running("c1"), None);
    }

    #[test]
    fn a_frame_reaches_a_watcher() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        let mut watch = hub.watch("c1").expect("a running turn is watchable");
        assert_eq!(watch.answer, "answer-1");
        turn.publish("delta", serde_json::json!({ "text": "hi" }));
        let frame = watch.frames.try_recv().expect("the frame arrived");
        assert_eq!(frame.event, "delta");
        assert_eq!(frame.data["text"], "hi");
    }

    #[test]
    fn a_frame_published_before_a_watcher_arrives_is_not_replayed() {
        // The page paints the row first and applies frames after it, so a frame
        // that predates the subscription is already in the row it read. Replaying
        // it would double the text.
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        turn.publish("delta", serde_json::json!({ "text": "before" }));
        let mut watch = hub.watch("c1").expect("still running");
        assert!(
            watch.frames.try_recv().is_err(),
            "subscribing does not replay what the row already carries"
        );
        turn.publish("delta", serde_json::json!({ "text": "after" }));
        assert_eq!(
            watch
                .frames
                .try_recv()
                .expect("the later frame arrives")
                .data["text"],
            "after"
        );
    }

    #[test]
    fn a_turn_with_no_watcher_reports_it_and_publishing_is_harmless() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        assert!(!turn.watched());
        turn.publish("delta", serde_json::json!({ "text": "nobody home" }));
        let watch = hub.watch("c1").expect("still running");
        assert_eq!(
            watch.frames.len(),
            0,
            "a page that arrives after that frame does not get it"
        );
    }

    #[test]
    fn the_table_counts_conversations_not_turns() {
        let hub = LiveHub::new();
        let one = hub.begin("c1", "a1");
        let two = hub.begin("c2", "a2");
        assert_eq!(hub.len(), 2);
        drop(one);
        assert_eq!(hub.len(), 1);
        drop(two);
        assert!(hub.is_empty());
    }

    #[test]
    fn frames_are_numbered_from_one_and_replay_after_the_one_the_page_saw() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        turn.publish("delta", serde_json::json!({ "text": "a" }));
        turn.publish("delta", serde_json::json!({ "text": "b" }));
        turn.publish("delta", serde_json::json!({ "text": "c" }));

        // A page that saw the first two gets the third and nothing it already has.
        let watch = hub.watch_after("c1", Some(2)).expect("still running");
        assert!(!watch.missed);
        assert_eq!(watch.replay.len(), 1, "only what was missed");
        assert_eq!(watch.replay[0].seq, 3);
        assert_eq!(watch.replay[0].data["text"], "c");
    }

    #[test]
    fn a_page_that_saw_everything_replays_nothing() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        turn.publish("delta", serde_json::json!({ "text": "a" }));
        let watch = hub.watch_after("c1", Some(1)).expect("still running");
        assert!(!watch.missed);
        assert!(watch.replay.is_empty(), "nothing to catch up on");
    }

    #[test]
    fn a_page_further_behind_than_the_ring_is_told_rather_than_patched() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        for index in 0..(BACKLOG + 5) {
            turn.publish("delta", serde_json::json!({ "text": index.to_string() }));
        }
        // The oldest frame the ring still holds is number 6, so a page whose last
        // frame was number 1 has a hole it cannot be caught up across.
        let watch = hub.watch_after("c1", Some(1)).expect("still running");
        assert!(watch.missed, "a gap in the ring is reported, not filled silently");
        // A page that is merely one frame behind is still served from the ring.
        let watch = hub.watch_after("c1", Some((BACKLOG + 4) as u64)).expect("still running");
        assert!(!watch.missed);
        assert_eq!(watch.replay.len(), 1);
    }

    #[test]
    fn the_ring_stops_growing_at_the_bound() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        for index in 0..(BACKLOG * 2) {
            turn.publish("delta", serde_json::json!({ "text": index.to_string() }));
        }
        // A page whose last frame is the one before the ring begins is served the
        // whole ring and no more: 257 through 512, which is BACKLOG frames.
        let watch = hub.watch_after("c1", Some(256)).expect("still running");
        assert!(!watch.missed);
        assert_eq!(watch.replay.len(), BACKLOG, "the ring holds exactly its bound");
        assert_eq!(
            watch.replay.first().map(|frame| frame.seq),
            Some(257),
            "and it begins at the oldest frame it kept"
        );
        // One frame further behind and the ring cannot span the gap.
        assert!(
            hub.watch_after("c1", Some(255))
                .expect("still running")
                .missed
        );
    }

    #[test]
    fn a_detached_reporter_numbers_its_frames_the_same_way() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        let reporter = turn.reporter();
        turn.publish("delta", serde_json::json!({ "text": "one" }));
        reporter.publish("subagent", serde_json::json!({ "status": "running" }));
        turn.publish("delta", serde_json::json!({ "text": "two" }));
        let watch = hub.watch_after("c1", Some(0)).expect("still running");
        let seqs: Vec<u64> = watch.replay.iter().map(|frame| frame.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "one sequence per conversation, whichever half sent it");
    }

    #[test]
    fn a_detached_reporter_publishes_but_cannot_end_the_turn() {
        let hub = LiveHub::new();
        let turn = hub.begin("c1", "answer-1");
        let mut watch = hub.watch("c1").expect("a running turn is watchable");
        let reporter = turn.reporter();
        drop(reporter.clone());
        assert_eq!(
            hub.running("c1").as_deref(),
            Some("answer-1"),
            "a cloned publisher is not the turn"
        );
        reporter.publish("subagent", serde_json::json!({ "status": "running" }));
        assert_eq!(
            watch.frames.try_recv().expect("published").event,
            "subagent"
        );
    }
}
