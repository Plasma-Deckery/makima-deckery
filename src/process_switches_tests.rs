use super::*;

#[test]
fn pause_is_shared_and_seen_by_a_later_subscriber() {
    let switches = Switches::default();
    let copy = switches.clone();
    switches.pause.set(true, "test");
    assert!(copy.pause.is_paused());
    let rx = copy.pause.subscribe();
    assert!(*rx.borrow(), "a reader built now starts paused");
}

#[tokio::test]
async fn a_gaming_request_waits_for_a_reader() {
    let switches = Switches::default();
    switches.gaming.request(true, "test");
    let queue = switches.clone().gaming.queue();
    assert_eq!(queue.lock().await.try_recv(), Ok(true));
}
