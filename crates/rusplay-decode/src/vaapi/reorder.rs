use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Depth {
    Fixed(usize),
    Learned { ceiling: usize },
}

impl Depth {
    pub(super) fn capacity(self) -> usize {
        match self {
            Self::Fixed(frames) => frames,
            Self::Learned { ceiling } => ceiling,
        }
    }

    fn initial(self) -> usize {
        match self {
            Self::Fixed(frames) => frames,
            Self::Learned { .. } => 0,
        }
    }
}

#[derive(Debug)]
pub(super) struct Reorder<T> {
    policy: Depth,
    depth: usize,
    held: Vec<(i32, T)>,
    ready: VecDeque<T>,
    released: Option<i32>,
}

impl<T> Reorder<T> {
    pub(super) fn new() -> Self {
        Self {
            policy: Depth::Fixed(0),
            depth: 0,
            held: Vec::new(),
            ready: VecDeque::new(),
            released: None,
        }
    }

    pub(super) fn configure(&mut self, policy: Depth) {
        if self.policy == policy {
            return;
        }

        self.flush();
        self.policy = policy;
        self.depth = policy.initial();
    }

    pub(super) fn push(&mut self, key: i32, item: T) {
        if self.released.is_some_and(|released| key < released) {
            self.learn();
            return;
        }

        self.held.push((key, item));
        while self.held.len() > self.depth {
            self.release_earliest();
        }
    }

    pub(super) fn pop(&mut self) -> Option<T> {
        while self.ready.len() > self.depth + 1 {
            self.ready.pop_front();
        }

        self.ready.pop_front()
    }

    pub(super) fn flush(&mut self) {
        self.held.sort_by_key(|&(key, _)| key);
        self.ready.extend(self.held.drain(..).map(|(_, item)| item));
        self.released = None;
    }

    pub(super) fn drain(&mut self) -> Vec<T> {
        self.flush();
        self.ready.drain(..).collect()
    }

    fn release_earliest(&mut self) {
        let Some(index) = self
            .held
            .iter()
            .enumerate()
            .min_by_key(|(_, (key, _))| *key)
            .map(|(index, _)| index)
        else {
            return;
        };

        let (key, item) = self.held.remove(index);
        self.released = Some(key);
        self.ready.push_back(item);
    }

    fn learn(&mut self) {
        if let Depth::Learned { ceiling } = self.policy {
            self.depth = (self.depth + 1).min(ceiling);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(reorder: &mut Reorder<i32>, keys: &[i32]) -> Vec<Option<i32>> {
        keys.iter()
            .map(|&key| {
                reorder.push(key, key);
                reorder.pop()
            })
            .collect()
    }

    #[test]
    fn zero_depth_passes_every_frame_straight_through() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(0));

        let outputs = feed(&mut reorder, &[0, 2, 4, 6]);

        assert_eq!(outputs, [Some(0), Some(2), Some(4), Some(6)]);
        assert_eq!(reorder.drain(), Vec::<i32>::new());
    }

    #[test]
    fn fixed_depth_delays_by_exactly_that_many_frames() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(1));

        let outputs = feed(&mut reorder, &[0, 6, 2, 4, 12, 8, 10]);

        assert_eq!(
            outputs,
            [None, Some(0), Some(2), Some(4), Some(6), Some(8), Some(10)]
        );
        assert_eq!(reorder.drain(), [12]);
    }

    #[test]
    fn pyramid_reorders_with_depth_two() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(2));

        let mut outputs = feed(&mut reorder, &[0, 8, 4, 2, 6, 16, 12, 10, 14]);
        outputs.extend(reorder.drain().into_iter().map(Some));

        let order: Vec<i32> = outputs.into_iter().flatten().collect();
        assert_eq!(order, [0, 2, 4, 6, 8, 10, 12, 14, 16]);
    }

    #[test]
    fn flush_before_a_new_sequence_keeps_the_old_frames_first_without_growing_the_delay() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(2));

        let mut outputs = feed(&mut reorder, &[0, 8, 4]);
        reorder.flush();
        outputs.extend(feed(&mut reorder, &[0, 8, 4, 2, 6]));
        outputs.extend(reorder.drain().into_iter().map(Some));

        let order: Vec<i32> = outputs.into_iter().flatten().collect();
        assert_eq!(order, [0, 4, 8, 0, 2, 4, 6, 8]);
    }

    #[test]
    fn learned_depth_starts_at_zero_and_grows_on_a_late_frame() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Learned { ceiling: 4 });

        let first = feed(&mut reorder, &[0, 2, 6, 4]);
        let second = feed(&mut reorder, &[12, 8, 10, 18, 14, 16]);

        assert_eq!(first, [Some(0), Some(2), Some(6), None]);
        assert_eq!(
            second,
            [None, Some(8), Some(10), Some(12), Some(14), Some(16)]
        );
    }

    #[test]
    fn learned_depth_never_exceeds_its_ceiling() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Learned { ceiling: 1 });

        feed(&mut reorder, &[10, 8, 6, 4, 2]);

        assert!(reorder.depth <= 1);
    }

    #[test]
    fn several_pictures_in_one_packet_keep_only_the_newest() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(0));

        reorder.push(0, 0);
        reorder.push(2, 2);

        assert_eq!(reorder.pop(), Some(2));
        assert_eq!(reorder.pop(), None);
    }

    #[test]
    fn changing_the_policy_releases_what_was_held() {
        let mut reorder = Reorder::new();
        reorder.configure(Depth::Fixed(2));
        feed(&mut reorder, &[4, 2]);

        reorder.configure(Depth::Fixed(0));

        assert_eq!(reorder.drain(), [2, 4]);
    }
}
