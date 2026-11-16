use std::{
    cell::Cell,
    rc::Rc,
    time::Duration,
};

use freya_core::prelude::*;
use freya_sdk::timeout::*;

// How long the cursor stays shown, then hidden, while blinking
const BLINK_TIME: Duration = Duration::from_millis(530);
// Time until the blinking is started since the last reset (key down, mouse click)
const ANIMATION_TIMEOUT: Duration = Duration::from_millis(500);

/// A hook that manages the cursor blink with a typing timeout.
/// When the user types, the cursor stays visible. After the timeout elapses,
/// the cursor starts blinking again. It switches between shown and hidden
/// without fading, so a blink costs one frame instead of an animation.
pub fn use_cursor_blink(enable: bool, color: Color) -> (Timeout, Color) {
    let movement_timeout = use_timeout(|| ANIMATION_TIMEOUT);
    let blinking = enable && movement_timeout.elapsed();
    let shown = use_state(|| true);
    let wanted = use_hook(|| Rc::new(Cell::new(false)));
    wanted.set(blinking);

    use_hook(move || {
        let mut shown = shown;
        spawn(async move {
            loop {
                timer(BLINK_TIME).await;
                let now = *shown.peek();
                if wanted.get() {
                    shown.set(!now);
                } else if !now {
                    shown.set(true);
                }
            }
        });
    });

    let cursor_color = if !blinking || *shown.read() {
        color
    } else {
        color.with_a(0)
    };

    (movement_timeout, cursor_color)
}
