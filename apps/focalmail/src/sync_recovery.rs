//! Background reads recover independently of the window. Interactive writes are never replayed.
use std::time::{Duration, Instant};

pub(crate) struct Schedule {
    pub next: Instant,
    generation: u64,
    failures: u32,
    suspended: bool,
}
impl Schedule {
    pub fn new(now: Instant) -> Self {
        Self {
            next: now + Duration::from_secs(5),
            generation: 0,
            failures: 0,
            suspended: false,
        }
    }
    pub fn due(&self, now: Instant) -> Option<u64> {
        (!self.suspended && now >= self.next).then_some(self.generation)
    }
    pub fn request(&mut self, now: Instant, delay: Duration) {
        self.generation = self.generation.wrapping_add(1);
        self.failures = 0;
        self.next = now + delay;
    }
    pub fn sleep(&mut self, sleeping: bool, now: Instant) {
        self.suspended = sleeping;
        // Invalidate an in-flight completion so it cannot overwrite the wake request.
        self.request(now, Duration::from_secs(2));
    }
    pub fn complete(&mut self, generation: u64, now: Instant, failed: bool, cancelled: bool) {
        if generation != self.generation {
            return;
        }
        let seconds = if cancelled {
            5
        } else if failed {
            let seconds = (15u64 << self.failures.min(5)).min(300);
            self.failures = self.failures.saturating_add(1);
            seconds
        } else {
            self.failures = 0;
            300
        };
        self.next = now + Duration::from_secs(seconds);
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Event {
    Sleep(bool),
    NetworkRestored,
    NetworkLost,
}

/// Reconnect to the system bus if it or the subscribed service restarts.
pub(crate) fn watch_events(notify: impl Fn(Event) + Send + Sync + 'static) {
    let notify = std::sync::Arc::new(notify);
    for sleep_watch in [true, false] {
        let notify = notify.clone();
        std::thread::spawn(move || {
            let mut retry = 5;
            loop {
                if observe(sleep_watch, &*notify).is_err() {
                    eprintln!(
                        "FocalMail: {} recovery connection unavailable; retrying in {retry}s",
                        if sleep_watch { "sleep" } else { "network" }
                    );
                }
                // Periodic sync/retries remain available without login1 or NetworkManager.
                std::thread::sleep(Duration::from_secs(retry));
                retry = (retry * 2).min(300);
            }
        });
    }
}

fn observe(sleep_watch: bool, notify: &impl Fn(Event)) -> anyhow::Result<()> {
    let connection = zbus::blocking::connection::Builder::system()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let (service, path, interface, signal) = if sleep_watch {
        (
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
        )
    } else {
        (
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "StateChanged",
        )
    };
    let proxy = zbus::blocking::Proxy::new(&connection, service, path, interface)?;
    let signals = proxy.receive_signal(signal)?;
    let mut network = if sleep_watch {
        notify(Event::Sleep(
            proxy.get_property::<bool>("PreparingForSleep")?,
        ));
        None
    } else {
        let state = proxy.get_property::<u32>("State")?;
        if state >= 50 {
            notify(Event::NetworkRestored);
        }
        Some(state)
    };
    eprintln!(
        "FocalMail: monitoring {} recovery",
        if sleep_watch {
            "suspend/resume"
        } else {
            "network"
        }
    );
    for message in signals {
        if sleep_watch {
            if let Ok((sleeping,)) = message.body().deserialize::<(bool,)>() {
                notify(Event::Sleep(sleeping));
            }
        } else if let Ok((state,)) = message.body().deserialize::<(u32,)>() {
            if let Some(event) = network_event(network, state) {
                notify(event);
            }
            network = Some(state);
        }
    }
    Ok(())
}

fn network_event(previous: Option<u32>, state: u32) -> Option<Event> {
    // Local/site connectivity can be enough for a private IMAP server. Also retry
    // when Internet access improves from local/site to global connectivity.
    match previous {
        Some(old) if state >= 50 && (old < 50 || state > old) => Some(Event::NetworkRestored),
        Some(old) if old >= 50 && state < 50 => Some(Event::NetworkLost),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_reads_back_off_and_success_resets_the_retry_window() {
        let mut now = Instant::now();
        let mut s = Schedule::new(now);
        assert!(s.due(now).is_none());
        now += Duration::from_secs(5);
        for delay in [15, 30, 60, 120, 240, 300, 300] {
            let generation = s.due(now).unwrap();
            s.complete(generation, now, true, false);
            assert_eq!(s.next.duration_since(now).as_secs(), delay);
            now = s.next;
        }
        s.complete(s.due(now).unwrap(), now, false, false);
        now = s.next;
        s.complete(s.due(now).unwrap(), now, true, false);
        assert_eq!(s.next.duration_since(now).as_secs(), 15);
    }
    #[test]
    fn wake_and_network_requests_survive_an_older_scan_completing() {
        let now = Instant::now() + Duration::from_secs(5);
        let mut s = Schedule::new(now - Duration::from_secs(5));
        let old = s.due(now).unwrap();
        s.sleep(true, now);
        assert!(s.due(now + Duration::from_secs(3600)).is_none());
        s.sleep(false, now);
        let wake = s.next;
        s.complete(old, now, true, false);
        assert_eq!(s.next, wake);
        assert!(s.due(wake).is_some());
        let old = s.due(wake).unwrap();
        s.request(wake, Duration::from_secs(2));
        s.complete(old, wake, false, false);
        assert_eq!(s.next, wake + Duration::from_secs(2));
    }
    #[test]
    fn network_recovery_detects_improving_access_without_repeated_retries() {
        assert!(matches!(
            network_event(Some(20), 50),
            Some(Event::NetworkRestored)
        ));
        assert!(matches!(
            network_event(Some(50), 70),
            Some(Event::NetworkRestored)
        ));
        assert!(matches!(
            network_event(Some(70), 20),
            Some(Event::NetworkLost)
        ));
        assert!(network_event(Some(70), 70).is_none());
        assert!(network_event(None, 70).is_none());
    }
}
