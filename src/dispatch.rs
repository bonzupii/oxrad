// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! One off-thread job queue for every parsed readout.
//!
//! A worker closure runs on the shared queue and calls the same registered
//! operation through [`Acquire`]. It owns no socket, cache or retry policy; the
//! engine remains the only request path.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::acquire::Acquire;

/// One owned worker closure accepted by the shared queue.
pub(crate) type Job = Box<dyn FnOnce(&Acquire) + Send + 'static>;

/// Why a job could not be accepted by the shared queue.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Disconnected;

impl Display for Disconnected {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("the shared job queue stopped")
    }
}

impl Error for Disconnected {}

/// A typed answer channel attached to the shared job queue.
pub(crate) struct Feed<E> {
    acquire: Acquire,
    answers: Sender<E>,
    events: Receiver<E>,
}

impl<E: Send + 'static> Feed<E> {
    pub(crate) fn new(acquire: Acquire) -> Self {
        let (answers, events) = mpsc::channel();
        Self {
            acquire,
            answers,
            events,
        }
    }

    /// Queues one owned worker closure. The closure may borrow nothing from
    /// the UI and returns the one answer the caller will poll.
    pub(crate) fn request<F>(&self, work: F) -> Result<(), Disconnected>
    where
        F: FnOnce(&Acquire) -> E + Send + 'static,
    {
        let answers = self.answers.clone();
        self.acquire.offload(move |acquire| {
            let answer = work(acquire);
            // A closed receiver means the readout was dropped; the answer has
            // no consumer and must not resurrect state.
            drop(answers.send(answer));
        })
    }

    /// Queues one closure that may emit several ordered answers through the
    /// feed's channel, as a tile plan does.
    pub(crate) fn request_stream<F>(&self, work: F) -> Result<(), Disconnected>
    where
        F: FnOnce(&Acquire, &Sender<E>) + Send + 'static,
    {
        let answers = self.answers.clone();
        self.acquire.offload(move |acquire| work(acquire, &answers))
    }

    /// A feed and its sender for tests that need to deliver an exact answer
    /// sequence without a live queue.
    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, Sender<E>) {
        let (answers, events) = mpsc::channel();
        let sender = answers.clone();
        (
            Self {
                acquire: Acquire::offline(),
                answers,
                events,
            },
            sender,
        )
    }

    /// Returns the next answer, or `Ok(None)` when the queue is still working.
    pub(crate) fn poll(&self) -> Result<Option<E>, Disconnected> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(Disconnected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    use crate::config::EnabledProviders;

    #[test]
    fn a_feed_delivers_one_typed_answer() -> Result<(), Disconnected> {
        let acquire = Acquire::new(EnabledProviders::all_admitted());
        let feed = Feed::new(acquire);
        feed.request(|_acquire| 7_u32)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(value) = feed.poll()? {
                assert_eq!(value, 7);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the shared queue did not answer");
    }

    #[test]
    fn a_dropped_feed_does_not_stop_the_queue() -> Result<(), Disconnected> {
        let acquire = Acquire::new(EnabledProviders::all_admitted());
        {
            let feed = Feed::new(acquire.clone());
            feed.request(|_acquire| 1_u32)?;
        }
        let (sender, receiver) = channel();
        acquire.offload(move |_acquire| {
            assert!(sender.send(2_u32).is_ok());
        })?;
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).ok(),
            Some(2_u32)
        );
        Ok(())
    }
}
