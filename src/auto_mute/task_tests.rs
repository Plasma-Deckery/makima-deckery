use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;
use tokio::time::Duration;

const GAME: u32 = 1000;

/// What the loop did to the world.
#[derive(Default)]
struct Log {
    subscribes: usize,
    listings: usize,
    applied: Vec<Mute>,
    saves: usize,
    /// The write ends of live subscriptions, held so they do not end.
    writers: Vec<tokio::io::DuplexStream>,
}

/// Set when a subscription's guard is dropped — the stand-in for `pactl`
/// being killed.
struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct Fake {
    enabled: Arc<AtomicBool>,
    log: Arc<Mutex<Log>>,
    /// A subscription ends the moment it starts, like a `pactl` that cannot
    /// reach PipeWire.
    exits_at_once: bool,
    stopped: Arc<AtomicBool>,
}

impl Fake {
    fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            log: Arc::default(),
            exits_at_once: false,
            stopped: Arc::default(),
        }
    }
}

impl Backend for Fake {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn subscribe(&mut self) -> std::io::Result<Subscription> {
        let mut log = self.log.lock().unwrap();
        log.subscribes += 1;
        let reader: Box<dyn AsyncBufRead + Unpin + Send> = if self.exits_at_once {
            Box::new(tokio::io::empty())
        } else {
            let (read, write) = tokio::io::duplex(64);
            log.writers.push(write);
            Box::new(tokio::io::BufReader::new(read))
        };
        self.stopped.store(false, Ordering::SeqCst);
        Ok(Subscription { lines: reader.lines(), _guard: Box::new(Guard(self.stopped.clone())) })
    }

    async fn list_streams(&mut self) -> Vec<Stream> {
        self.log.lock().unwrap().listings += 1;
        vec![Stream { index: 10, game: Some(GAME), app: "Hades".into(), muted: false }]
    }

    async fn apply(&mut self, actions: &[Mute], _streams: &[Stream]) {
        self.log.lock().unwrap().applied.extend_from_slice(actions);
    }

    fn save_ours(&mut self, _ours: &BTreeSet<String>) {
        self.log.lock().unwrap().saves += 1;
    }
}

fn focus(tx: &watch::Sender<GameFocus>, game: Option<u32>) {
    tx.send_replace(GameFocus { game, big_picture: false });
}

/// Let the loop catch up. Time is paused, so this costs nothing real.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

/// The bug: with the feature off, every focus change still ran
/// `pactl list sink-inputs` and walked `/proc`.
#[tokio::test(start_paused = true)]
async fn a_disabled_config_spawns_nothing() {
    let fake = Fake::new(false);
    let (tx, rx) = watch::channel(GameFocus::default());
    let task = tokio::spawn(run(fake.clone(), BTreeSet::new(), rx));

    for game in [Some(GAME), None, Some(GAME), None] {
        focus(&tx, game);
        settle().await;
    }
    drop(tx);
    task.await.unwrap();

    let log = fake.log.lock().unwrap();
    assert_eq!(log.subscribes, 0, "started pactl subscribe while off");
    assert_eq!(log.listings, 0, "listed streams while off");
    assert!(log.applied.is_empty());
    assert_eq!(log.saves, 0);
}

/// On, it subscribes once and acts on a focus change.
#[tokio::test(start_paused = true)]
async fn enabled_it_subscribes_and_mutes_the_background_game() {
    let fake = Fake::new(true);
    let (tx, rx) = watch::channel(GameFocus::default());
    let _task = tokio::spawn(run(fake.clone(), BTreeSet::new(), rx));

    focus(&tx, Some(GAME));
    settle().await;
    focus(&tx, None);
    settle().await;

    let log = fake.log.lock().unwrap();
    assert_eq!(log.subscribes, 1);
    assert_eq!(log.applied, vec![Mute::On(10)]);
}

/// Turning it off releases what it muted — one listing for that — then stops
/// `pactl subscribe` and lists nothing more.
#[tokio::test(start_paused = true)]
async fn turning_it_off_stops_everything_after_releasing() {
    let fake = Fake::new(true);
    let (tx, rx) = watch::channel(GameFocus::default());
    let _task = tokio::spawn(run(fake.clone(), BTreeSet::new(), rx));

    focus(&tx, Some(GAME));
    settle().await;
    focus(&tx, None);
    settle().await;
    assert!(!fake.stopped.load(Ordering::SeqCst));

    fake.enabled.store(false, Ordering::SeqCst);
    focus(&tx, None);
    settle().await;
    let listings = fake.log.lock().unwrap().listings;
    assert_eq!(fake.log.lock().unwrap().applied, vec![Mute::On(10), Mute::Off(10)]);
    assert!(fake.stopped.load(Ordering::SeqCst), "pactl subscribe kept running while off");

    for game in [Some(GAME), None] {
        focus(&tx, game);
        settle().await;
    }
    let log = fake.log.lock().unwrap();
    assert_eq!(log.listings, listings, "listed streams while off");
    assert_eq!(log.subscribes, 1);
}

/// A `pactl subscribe` that exits at once is restarted on the backoff
/// schedule, not in a busy loop: attempts at 0, 0.5, 1.5, 3.5 and 7.5 s.
#[tokio::test(start_paused = true)]
async fn an_exiting_subscribe_is_restarted_with_backoff() {
    let mut fake = Fake::new(true);
    fake.exits_at_once = true;
    let (_tx, rx) = watch::channel(GameFocus::default());
    let _task = tokio::spawn(run(fake.clone(), BTreeSet::new(), rx));

    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(fake.log.lock().unwrap().subscribes, 5);
}
