//! What the operating system does to the manager window that Freya does not act on itself: minimizing, covering and
//! focusing, passed to the store as [`Signal`]s.

use std::cell::RefCell;

use freya::winit::event::WindowEvent;
use freya_winit::plugins::{FreyaPlugin, PluginEvent, PluginHandle};

/// A change of the manager window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Whether nobody can see the window: minimized or fully covered.
    Hidden(bool),
    /// Whether the window has the keyboard focus.
    Focused(bool),
}

thread_local! {
    static CHANNEL: RefCell<Option<(async_channel::Sender<Signal>, async_channel::Receiver<Signal>)>> = const { RefCell::new(None) };
}

fn channel() -> (async_channel::Sender<Signal>, async_channel::Receiver<Signal>) {
    CHANNEL.with_borrow_mut(|c| c.get_or_insert_with(async_channel::unbounded).clone())
}

/// The signals of the manager window, those that came before the call included.
pub fn listen() -> async_channel::Receiver<Signal> {
    channel().1
}

fn send(signal: Signal) {
    let _ = channel().0.try_send(signal);
}

/// Follows the window events that tell whether the manager is seen and in front.
#[derive(Default)]
pub struct Watch {
    minimized: bool,
    occluded: bool,
}

impl Watch {
    fn hidden(&self) -> bool {
        self.minimized || self.occluded
    }
}

impl FreyaPlugin for Watch {
    fn plugin_id(&self) -> &'static str {
        "manycraft-signals"
    }

    fn on_event(&mut self, event: &mut PluginEvent, _: PluginHandle) {
        let PluginEvent::WindowEvent { window, event } = event else { return };
        let was = self.hidden();
        match event {
            WindowEvent::Resized(size) => self.minimized = window.is_minimized().unwrap_or(size.width == 0 || size.height == 0),
            WindowEvent::Occluded(covered) => self.occluded = *covered,
            WindowEvent::Focused(true) => {
                (self.minimized, self.occluded) = (false, false);
                send(Signal::Focused(true));
            }
            WindowEvent::Focused(false) => send(Signal::Focused(false)),
            _ => return,
        }
        if self.hidden() != was {
            send(Signal::Hidden(self.hidden()));
        }
    }
}
