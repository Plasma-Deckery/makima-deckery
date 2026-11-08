use super::*;

const MS: u64 = 1000;

/// Big Picture is a gaming focus that needs no `/proc` walk, so the tests can
/// stand in a game without a real process tree.
fn big_picture() -> Client {
    Client::Class("steam".into(), "Steam Big Picture".into(), None)
}

fn desktop() -> Client {
    Client::Class("org.kde.dolphin".into(), "Home — Dolphin".into(), None)
}

/// A running focus task with time stopped, plus the handles to drive it.
struct Rig {
    notify: Arc<Notify>,
    client: Arc<Mutex<Client>>,
    rx: watch::Receiver<GameFocus>,
}

impl Rig {
    async fn start(threshold_ms: u64) -> Self {
        let notify = Arc::new(Notify::new());
        let client = Arc::new(Mutex::new(Client::Default));
        let (tx, rx) = watch::channel(GameFocus::default());
        tokio::spawn(game_focus_task(
            notify.clone(),
            client.clone(),
            move || Duration::from_millis(threshold_ms),
            tx,
        ));
        // Let the task register for the first notification.
        settle().await;
        Rig { notify, client, rx }
    }

    async fn focus(&self, client: Client) {
        *self.client.lock().await = client;
        self.notify.notify_waiters();
        settle().await;
    }

    fn gaming(&self) -> bool {
        self.rx.borrow().is_gaming()
    }
}

/// Run every ready task, without moving the clock.
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

async fn after(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

#[test]
fn only_gaining_a_game_waits() {
    let game = GameFocus { game: Some(42), big_picture: false };
    let none = GameFocus::default();
    let threshold = Duration::from_millis(MS);

    assert_eq!(wait_before_publishing(game, threshold), Some(threshold));
    assert_eq!(wait_before_publishing(none, threshold), None, "leaving must not wait");
    assert_eq!(wait_before_publishing(game, Duration::ZERO), None, "0 means at once");
}

/// The case this exists for: a game keeps focus for the whole threshold and
/// then counts — not before.
#[tokio::test(start_paused = true)]
async fn a_game_counts_after_keeping_focus_for_the_threshold() {
    let rig = Rig::start(MS).await;
    rig.focus(big_picture()).await;

    after(MS - 1).await;
    assert!(!rig.gaming(), "counted before the threshold");
    after(1).await;
    assert!(rig.gaming(), "did not count after the threshold");
}

/// Sliding past a game's desktop: focus lands on it and moves on before the
/// threshold. It must never count — that is what trapped the controller.
#[tokio::test(start_paused = true)]
async fn passing_over_a_game_never_counts() {
    let mut rig = Rig::start(MS).await;
    rig.rx.borrow_and_update();
    rig.focus(big_picture()).await;
    after(300).await;
    // This moment is the bug: without the threshold, Gaming Mode would already
    // be on here and the L1+D-pad that is moving on would no longer work.
    assert!(!rig.gaming(), "counted while only passing over");
    rig.focus(desktop()).await;
    after(5 * MS).await;
    // Not merely off now — never published as gaming at any point.
    assert!(!rig.rx.has_changed().unwrap(), "published a gaming focus on the way past");
}

/// Leaving a game counts at once. Waiting on the way out would leave the
/// controller raw on the desktop.
#[tokio::test(start_paused = true)]
async fn leaving_a_game_counts_at_once() {
    let rig = Rig::start(MS).await;
    rig.focus(big_picture()).await;
    after(MS).await;
    assert!(rig.gaming());

    rig.focus(desktop()).await;
    assert!(!rig.gaming(), "leaving waited for the threshold");
}

/// Coming back restarts the clock — the earlier, interrupted wait does not count
/// towards it.
#[tokio::test(start_paused = true)]
async fn returning_starts_the_wait_again() {
    let rig = Rig::start(MS).await;
    rig.focus(big_picture()).await;
    after(800).await;
    rig.focus(desktop()).await;
    rig.focus(big_picture()).await;

    after(800).await;
    assert!(!rig.gaming(), "carried the interrupted wait over");
    after(200).await;
    assert!(rig.gaming());
}

/// With no threshold the behaviour is what it was before: immediate.
#[tokio::test(start_paused = true)]
async fn a_zero_threshold_is_immediate() {
    let rig = Rig::start(0).await;
    rig.focus(big_picture()).await;
    assert!(rig.gaming());
}
