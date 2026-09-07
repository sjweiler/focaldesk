use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use focaldesk_types::OutputId;
use smithay::utils::{Physical, Rectangle};

use super::{CaptureFrame, CaptureGeometry};

/// Capture queues stay deliberately shallow so a stalled consumer cannot retain
/// an unbounded number of GPU buffers.
pub const MAX_CAPTURE_QUEUE_DEPTH: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CaptureConsumerId(u64);

struct CaptureConsumer<B> {
    output_id: OutputId,
    queue_limit: usize,
    queue: VecDeque<CaptureFrame<B>>,
    last_geometry: Option<CaptureGeometry>,
    needs_full_refresh: bool,
}

/// Fans compositor-owned output frames out to independently bounded consumers.
pub struct OutputCaptureBroker<B> {
    next_consumer_id: u64,
    output_serials: HashMap<OutputId, u64>,
    consumers: HashMap<CaptureConsumerId, CaptureConsumer<B>>,
}

impl<B> Default for OutputCaptureBroker<B> {
    fn default() -> Self {
        Self {
            next_consumer_id: 1,
            output_serials: HashMap::new(),
            consumers: HashMap::new(),
        }
    }
}

impl<B> OutputCaptureBroker<B> {
    pub fn register(&mut self, output_id: OutputId, queue_depth: usize) -> CaptureConsumerId {
        let id = CaptureConsumerId(self.next_consumer_id);
        self.next_consumer_id = self.next_consumer_id.saturating_add(1);
        self.consumers.insert(
            id,
            CaptureConsumer {
                output_id,
                queue_limit: queue_depth.clamp(1, MAX_CAPTURE_QUEUE_DEPTH),
                queue: VecDeque::new(),
                last_geometry: None,
                needs_full_refresh: true,
            },
        );
        id
    }

    pub fn remove(&mut self, consumer_id: CaptureConsumerId) -> bool {
        self.consumers.remove(&consumer_id).is_some()
    }

    pub fn contains(&self, consumer_id: CaptureConsumerId) -> bool {
        self.consumers.contains_key(&consumer_id)
    }

    pub fn latest_frame(&self, consumer_id: CaptureConsumerId) -> Option<&CaptureFrame<B>> {
        self.consumers.get(&consumer_id)?.queue.back()
    }

    pub fn poll_frame(&mut self, consumer_id: CaptureConsumerId) -> Option<CaptureFrame<B>> {
        self.consumers.get_mut(&consumer_id)?.queue.pop_front()
    }

    /// Drop obsolete frames and return the newest one. Skipping incremental frames
    /// forces a full refresh so the consumer can recover without preserving history.
    pub fn take_latest_frame(&mut self, consumer_id: CaptureConsumerId) -> Option<CaptureFrame<B>> {
        let consumer = self.consumers.get_mut(&consumer_id)?;
        let skipped = consumer.queue.len() > 1;
        let mut frame = consumer.queue.pop_back()?;
        consumer.queue.clear();
        if skipped {
            frame.force_full_refresh();
        }
        Some(frame)
    }

    pub fn invalidate_output(&mut self, output_id: OutputId) {
        self.output_serials.remove(&output_id);
        for consumer in self
            .consumers
            .values_mut()
            .filter(|consumer| consumer.output_id == output_id)
        {
            consumer.queue.clear();
            consumer.last_geometry = None;
            consumer.needs_full_refresh = true;
        }
    }

    pub fn invalidate_all(&mut self) {
        self.output_serials.clear();
        for consumer in self.consumers.values_mut() {
            consumer.queue.clear();
            consumer.last_geometry = None;
            consumer.needs_full_refresh = true;
        }
    }
}

impl<B: Clone> OutputCaptureBroker<B> {
    pub fn publish(
        &mut self,
        output_id: OutputId,
        buffer: B,
        geometry: CaptureGeometry,
        damage: Vec<Rectangle<i32, Physical>>,
        captured_at: Instant,
    ) -> u64 {
        let serial = self.output_serials.entry(output_id).or_insert(0);
        *serial = serial.saturating_add(1);
        let serial = *serial;

        for consumer in self
            .consumers
            .values_mut()
            .filter(|consumer| consumer.output_id == output_id)
        {
            let geometry_changed = consumer.last_geometry != Some(geometry);
            let queue_was_full = consumer.queue.len() >= consumer.queue_limit;
            if queue_was_full {
                consumer.queue.pop_front();
            }

            let mut frame = CaptureFrame {
                output_id,
                serial,
                captured_at,
                geometry,
                buffer: buffer.clone(),
                damage: damage.clone(),
                full_refresh: false,
            };
            if consumer.needs_full_refresh || geometry_changed || queue_was_full {
                frame.force_full_refresh();
            }
            consumer.needs_full_refresh = false;
            consumer.last_geometry = Some(geometry);
            consumer.queue.push_back(frame);
        }

        serial
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use focaldesk_types::OutputId;
    use smithay::utils::{Physical, Rectangle, Size, Transform};

    use super::*;

    fn geometry(width: i32) -> CaptureGeometry {
        CaptureGeometry {
            size: Size::from((width, 100)),
            scale: 1.0,
            transform: Transform::Normal,
        }
    }

    fn damage() -> Vec<Rectangle<i32, Physical>> {
        vec![Rectangle::from_loc_and_size((5, 5), (10, 10))]
    }

    #[test]
    fn registration_and_removal_are_isolated() {
        let mut broker = OutputCaptureBroker::<u8>::default();
        let first = broker.register(OutputId(1), 1);
        let second = broker.register(OutputId(2), 1);
        broker.publish(OutputId(1), 7, geometry(100), damage(), Instant::now());

        assert!(broker.latest_frame(first).is_some());
        assert!(broker.latest_frame(second).is_none());
        assert!(broker.remove(first));
        assert!(!broker.contains(first));
        assert!(!broker.remove(first));
    }

    #[test]
    fn first_and_geometry_changed_frames_are_full_refreshes() {
        let mut broker = OutputCaptureBroker::<u8>::default();
        let consumer = broker.register(OutputId(1), 2);
        broker.publish(OutputId(1), 1, geometry(100), damage(), Instant::now());
        assert!(broker.poll_frame(consumer).unwrap().full_refresh);

        broker.publish(OutputId(1), 2, geometry(100), damage(), Instant::now());
        assert!(!broker.poll_frame(consumer).unwrap().full_refresh);

        broker.publish(OutputId(1), 3, geometry(200), damage(), Instant::now());
        let changed = broker.poll_frame(consumer).unwrap();
        assert!(changed.full_refresh);
        assert_eq!(changed.damage[0].size, geometry(200).size);
    }

    #[test]
    fn queue_is_bounded_and_overflow_recovers_with_full_frame() {
        let mut broker = OutputCaptureBroker::<u8>::default();
        let consumer = broker.register(OutputId(1), usize::MAX);
        for buffer in 0..4 {
            broker.publish(OutputId(1), buffer, geometry(100), damage(), Instant::now());
        }

        let first = broker.poll_frame(consumer).unwrap();
        let second = broker.poll_frame(consumer).unwrap();
        assert_eq!((first.buffer, second.buffer), (2, 3));
        assert!(first.full_refresh);
        assert!(second.full_refresh);
        assert!(broker.poll_frame(consumer).is_none());
    }

    #[test]
    fn taking_latest_after_skips_forces_recovery() {
        let mut broker = OutputCaptureBroker::<u8>::default();
        let consumer = broker.register(OutputId(1), 2);
        broker.publish(OutputId(1), 1, geometry(100), damage(), Instant::now());
        broker.publish(OutputId(1), 2, geometry(100), damage(), Instant::now());

        let latest = broker.take_latest_frame(consumer).unwrap();
        assert_eq!(latest.buffer, 2);
        assert!(latest.full_refresh);
        assert!(broker.poll_frame(consumer).is_none());
    }
}
