//! Intended HID state. This protocol has no device acknowledgement for input.
use crate::Transition;
use alloc::{collections::BTreeSet, vec, vec::Vec};
finite_states! {
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerMode { Absolute, Relative }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidEvent {
    Key {
        code: u16,
        pressed: bool,
    },
    Consumer(u16),
    Move {
        x: i16,
        y: i16,
        mode: PointerMode,
    },
    Buttons {
        x: i16,
        y: i16,
        buttons: u8,
    },
    Button {
        x: i16,
        y: i16,
        button: u8,
        pressed: bool,
    },
    Scroll {
        x: i16,
        y: i16,
        scroll: i8,
    },
    Reset,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidAction {
    Key {
        code: u16,
        pressed: bool,
    },
    Consumer(u16),
    Pointer {
        x: i16,
        y: i16,
        buttons: u8,
        scroll: i8,
        mode: PointerMode,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HidEffect {
    Send(Vec<HidAction>),
    Reject,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hid {
    keys: BTreeSet<u16>,
    x: i16,
    y: i16,
    buttons: u8,
    mode: PointerMode,
}
impl Default for Hid {
    fn default() -> Self {
        Self {
            keys: BTreeSet::new(),
            x: 0,
            y: 0,
            buttons: 0,
            mode: PointerMode::Absolute,
        }
    }
}
impl Hid {
    fn pointer(&self, x: i16, y: i16, scroll: i8) -> HidAction {
        HidAction::Pointer {
            x,
            y,
            buttons: self.buttons,
            scroll,
            mode: self.mode,
        }
    }
    pub fn step(mut self, event: HidEvent) -> Transition<Self, HidEffect> {
        let effect = match event {
            HidEvent::Key { code, pressed } => {
                if pressed {
                    self.keys.insert(code);
                } else {
                    self.keys.remove(&code);
                }
                HidEffect::Send(vec![HidAction::Key { code, pressed }])
            }
            HidEvent::Consumer(usage) => HidEffect::Send(vec![HidAction::Consumer(usage)]),
            HidEvent::Move { x, y, mode } => {
                self.mode = mode;
                if mode == PointerMode::Absolute {
                    self.x = x;
                    self.y = y;
                }
                HidEffect::Send(vec![self.pointer(x, y, 0)])
            }
            HidEvent::Buttons { x, y, buttons } => {
                if buttons > 7 {
                    HidEffect::Reject
                } else {
                    self.buttons = buttons;
                    self.x = x;
                    self.y = y;
                    HidEffect::Send(vec![self.pointer(x, y, 0)])
                }
            }
            HidEvent::Button {
                x,
                y,
                button,
                pressed,
            } => {
                if button > 2 {
                    HidEffect::Reject
                } else {
                    let bit = 1u8 << button;
                    if pressed {
                        self.buttons |= bit;
                    } else {
                        self.buttons &= !bit;
                    }
                    self.x = x;
                    self.y = y;
                    HidEffect::Send(vec![self.pointer(x, y, 0)])
                }
            }
            HidEvent::Scroll { x, y, scroll } => {
                if self.mode == PointerMode::Absolute {
                    self.x = x;
                    self.y = y;
                }
                HidEffect::Send(vec![self.pointer(x, y, scroll)])
            }
            HidEvent::Reset => {
                let mut actions: Vec<_> = self
                    .keys
                    .iter()
                    .map(|code| HidAction::Key {
                        code: *code,
                        pressed: false,
                    })
                    .collect();
                self.keys.clear();
                self.buttons = 0;
                // A relative reset releases buttons without moving the cursor.
                let (x, y) = match self.mode {
                    PointerMode::Absolute => (self.x, self.y),
                    PointerMode::Relative => (0, 0),
                };
                actions.push(self.pointer(x, y, 0));
                HidEffect::Send(actions)
            }
        };
        Transition {
            state: self,
            effect,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_button_release_preserves_other_buttons_and_invalid_inputs_preserve_intent() {
        let mut model = Hid::default();
        for (button, pressed, expected) in [(0, true, 1), (2, true, 5), (0, false, 4)] {
            model = model
                .step(HidEvent::Button {
                    x: 10,
                    y: 20,
                    button,
                    pressed,
                })
                .state;
            assert_eq!(model.buttons, expected);
        }
        for button in 3..=255 {
            let result = model.clone().step(HidEvent::Button {
                x: 0,
                y: 0,
                button,
                pressed: true,
            });
            assert_eq!(result.effect, HidEffect::Reject);
            assert_eq!(result.state, model);
        }
    }
    #[test]
    fn reset_is_retryable_after_partial_execution_and_obeys_current_pointer_mode() {
        for &mode in PointerMode::ALL {
            let model = Hid::default()
                .step(HidEvent::Key {
                    code: 4,
                    pressed: true,
                })
                .state
                .step(HidEvent::Move { x: 10, y: 20, mode })
                .state;
            let reset = model.clone().step(HidEvent::Reset);
            // The shell commits only after all outputs are enqueued; a failed
            // or cancelled effect can regenerate all releases from old intent.
            assert_eq!(reset, model.step(HidEvent::Reset));
            assert!(reset.state.keys.is_empty());
            let HidEffect::Send(actions) = reset.effect else {
                panic!("reset emits releases")
            };
            assert_eq!(
                actions[0],
                HidAction::Key {
                    code: 4,
                    pressed: false
                }
            );
            assert!(
                matches!(actions[1], HidAction::Pointer { mode: actual, buttons: 0, .. } if actual == mode)
            );
        }
    }
}
