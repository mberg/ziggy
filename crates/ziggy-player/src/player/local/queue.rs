//! The local player's queue: the songs of an album or artist, in order or shuffled.

use crate::model::Repeat;

/// A small random number source; good enough for shuffling songs.
pub struct Rng(u64);

impl Rng {
    pub fn seeded() -> Rng {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Rng(seed | 1)
    }

    pub fn below(&mut self, n: usize) -> usize {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

#[derive(Default)]
pub struct Queue {
    /// Track IDs in the album's own order.
    pub ids: Vec<String>,
    /// Positions in `ids`, in playing order.
    order: Vec<usize>,
    /// Where in `order` playback is.
    at: usize,
    pub shuffle: bool,
    pub repeat: Repeat,
}

impl Queue {
    /// Replaces the queue. Starts at `start` (an index into `ids`), or at a random song when
    /// shuffling without one.
    pub fn load(&mut self, ids: Vec<String>, start: Option<usize>, shuffle: bool, rng: &mut Rng) {
        self.ids = ids;
        self.shuffle = shuffle;
        let start = start.filter(|&s| s < self.ids.len()).unwrap_or_else(|| {
            if shuffle {
                rng.below(self.ids.len())
            } else {
                0
            }
        });
        self.order = (0..self.ids.len()).collect();
        self.at = start;
        if shuffle {
            self.reshuffle(rng);
        }
    }

    /// Shuffles everything but the current song, which goes first.
    fn reshuffle(&mut self, rng: &mut Rng) {
        let Some(&current) = self.order.get(self.at) else {
            return;
        };
        let mut rest: Vec<usize> = (0..self.ids.len()).filter(|&i| i != current).collect();
        for i in (1..rest.len()).rev() {
            rest.swap(i, rng.below(i + 1));
        }
        self.order = std::iter::once(current).chain(rest).collect();
        self.at = 0;
    }

    pub fn set_shuffle(&mut self, shuffle: bool, rng: &mut Rng) {
        if shuffle == self.shuffle {
            return;
        }
        self.shuffle = shuffle;
        if shuffle {
            self.reshuffle(rng);
        } else if let Some(&current) = self.order.get(self.at) {
            self.order = (0..self.ids.len()).collect();
            self.at = current;
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.order
            .get(self.at)
            .and_then(|&i| self.ids.get(i))
            .map(String::as_str)
    }

    /// Moves on. `finished` is true when the song ended by itself, so repeating one song
    /// plays it again; a tap on Next always moves on. None at the end of the queue.
    pub fn next(&mut self, finished: bool) -> Option<&str> {
        if self.order.is_empty() {
            return None;
        }
        if finished && self.repeat == Repeat::Track {
            return self.current();
        }
        if self.at + 1 < self.order.len() {
            self.at += 1;
        } else if self.repeat == Repeat::Off {
            return None;
        } else {
            self.at = 0;
        }
        self.current()
    }

    /// The song before; stays on the first one.
    pub fn previous(&mut self) -> Option<&str> {
        if self.at > 0 {
            self.at -= 1;
        } else if self.repeat != Repeat::Off && !self.order.is_empty() {
            self.at = self.order.len() - 1;
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| i.to_string()).collect()
    }

    #[test]
    fn plays_in_order_and_stops_at_the_end() {
        let mut q = Queue::default();
        q.load(ids(3), Some(1), false, &mut Rng::seeded());
        assert_eq!(q.current(), Some("1"));
        assert_eq!(q.next(true), Some("2"));
        assert_eq!(q.next(true), None);
        assert_eq!(q.previous(), Some("1"));
    }

    #[test]
    fn repeat_wraps_round_or_plays_one_song_again() {
        let mut q = Queue::default();
        q.load(ids(2), None, false, &mut Rng::seeded());
        q.repeat = Repeat::Context;
        assert_eq!(q.next(true), Some("1"));
        assert_eq!(q.next(true), Some("0"));
        q.repeat = Repeat::Track;
        assert_eq!(q.next(true), Some("0"));
        assert_eq!(q.next(false), Some("1"), "Next still moves on");
    }

    #[test]
    fn shuffle_plays_every_song_once_starting_with_the_current_one() {
        let mut rng = Rng::seeded();
        let mut q = Queue::default();
        q.load(ids(20), Some(5), true, &mut rng);
        assert_eq!(q.current(), Some("5"));
        let mut seen = vec![q.current().unwrap().to_string()];
        while let Some(id) = q.next(true) {
            seen.push(id.to_string());
        }
        seen.sort_by_key(|s| s.parse::<usize>().unwrap());
        assert_eq!(seen, ids(20));

        // Turning shuffle off carries on from the same song in album order.
        let mut q = Queue::default();
        q.load(ids(5), Some(3), true, &mut rng);
        q.set_shuffle(false, &mut rng);
        assert_eq!(q.current(), Some("3"));
        assert_eq!(q.next(true), Some("4"));
    }
}
