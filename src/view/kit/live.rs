use freya::prelude::*;

/// Writes to a state from a task that can outlive the component that owns it. `State::set` and `State::write` panic
/// when the state was dropped with its scope or is borrowed at that moment; these skip the write instead.
pub trait Live<T: 'static> {
    /// Changes the value in place, or does nothing when the state is gone or borrowed.
    fn soft(self, f: impl FnOnce(&mut T));
    /// Replaces the value, or does nothing when the state is gone or borrowed.
    fn settle(self, value: T);
    /// A copy of the value, or `None` when the state is gone or borrowed for writing.
    fn got(self) -> Option<T>
    where
        T: Clone;
}

impl<T: 'static> Live<T> for State<T> {
    fn soft(mut self, f: impl FnOnce(&mut T)) {
        if let Some(mut w) = self.try_write() {
            f(&mut w);
        }
    }

    fn settle(self, value: T) {
        self.soft(|w| *w = value);
    }

    fn got(self) -> Option<T>
    where
        T: Clone,
    {
        self.try_peek().map(|r| r.clone())
    }
}
