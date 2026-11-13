use std::io::{BufRead, BufReader};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use crate::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The stream (re)connected; refetch everything shown.
    Connected,
    /// The stream dropped and reconnecting failed; the app is probably offline until the next
    /// `Connected`. A drop followed by a successful immediate reconnect only reports `Connected`.
    Disconnected,
    Reminders,
    Notes,
}

/// The server pings every 20 s; a stream silent for this long is dead.
const IDLE: Duration = Duration::from_secs(45);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// A running `/v1/events` follower. Dropping it stops the thread and closes the stream.
pub struct Subscription {
    agent: ureq::Agent,
    /// Dropping the sender tells the thread to stop and wakes it from its backoff.
    stop: Option<mpsc::Sender<()>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop.take();
        // Closes the stream's socket, so a thread blocked reading it returns straight away.
        self.agent.clone().shutdown();
    }
}

/// Spawns a thread that follows `/v1/events`, reconnecting with backoff (1 s doubling to 5 s),
/// and calls `on_event` for each event until the returned [`Subscription`] is dropped.
pub fn subscribe(config: &Config, mut on_event: impl FnMut(Event) + Send + 'static) -> Subscription {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        // An idle limit, not a total: it restarts with every read. Pings arrive every 20 s.
        .timeout_per_read(Some(IDLE))
        .build()
        .new_agent();
    if !config.is_set() {
        return Subscription { agent, stop: None };
    }
    let url = format!("{}/v1/events", config.url.trim_end_matches('/'));
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread_agent = agent.clone();
    std::thread::spawn(move || {
        let stopped = || matches!(stop_rx.try_recv(), Err(TryRecvError::Disconnected));
        let mut backoff = Duration::from_secs(1);
        // Starts true so a first connection failure reports `Disconnected`.
        let mut reported_online = true;
        loop {
            let response = thread_agent.get(&url).header("Accept", "text/event-stream").call();
            if stopped() {
                return;
            }
            if let Ok(resp) = response
                && resp.status().is_success()
            {
                backoff = Duration::from_secs(1);
                reported_online = true;
                on_event(Event::Connected);
                read_stream(resp, &mut on_event, &stopped);
                if stopped() {
                    return;
                }
                // Reconnect straight away; only a failed reconnect counts as offline.
                continue;
            }
            if reported_online {
                reported_online = false;
                on_event(Event::Disconnected);
            }
            match stop_rx.recv_timeout(backoff) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
    Subscription { agent, stop: Some(stop_tx) }
}

fn read_stream(
    resp: ureq::http::Response<ureq::Body>,
    on_event: &mut impl FnMut(Event),
    stopped: &impl Fn() -> bool,
) {
    let reader = BufReader::new(resp.into_body().into_reader());
    let mut name = String::new();
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if stopped() {
            return;
        }
        if let Some(n) = line.strip_prefix("event:") {
            name = n.trim().to_string();
        } else if line.is_empty() && !name.is_empty() {
            let ev = match name.as_str() {
                "reminders" => Some(Event::Reminders),
                "notes" => Some(Event::Notes),
                _ => None,
            };
            name.clear();
            if let Some(ev) = ev {
                on_event(ev);
            }
        }
    }
}
