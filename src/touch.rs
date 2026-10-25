//! Touchscreens, for the windows snapr draws itself (the capture overlay,
//! pins, the toast). winit reports a finger only as touch events, never as
//! the mouse (on Windows it takes the touch messages, so the system doesn't
//! turn them into mouse ones), and those windows only follow the mouse. egui
//! windows don't need this: egui-winit does the same for them.

use winit::event::{ElementState, MouseButton, Touch, TouchPhase, WindowEvent};

/// Plays the first finger down as the mouse with its left button held.
/// Fingers that join it are left out, until all of them are lifted.
#[derive(Default)]
pub struct Finger {
    id: Option<u64>,
}

impl Finger {
    /// The mouse events a touch stands for, in order.
    pub fn mouse_events(&mut self, touch: &Touch) -> Vec<WindowEvent> {
        let device_id = touch.device_id;
        let moved = WindowEvent::CursorMoved {
            device_id,
            position: touch.location,
        };
        let button = |state| WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        };
        match touch.phase {
            TouchPhase::Started if self.id.is_none() => {
                self.id = Some(touch.id);
                vec![
                    WindowEvent::CursorEntered { device_id },
                    moved,
                    button(ElementState::Pressed),
                ]
            }
            _ if self.id != Some(touch.id) => Vec::new(),
            TouchPhase::Started | TouchPhase::Moved => vec![moved],
            // Lifted: the mouse goes too, as there's nothing left to hover.
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.id = None;
                vec![
                    moved,
                    button(ElementState::Released),
                    WindowEvent::CursorLeft { device_id },
                ]
            }
        }
    }
}
