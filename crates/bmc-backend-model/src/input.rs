//! Intended input state. Plan on an owned clone; commit only after atomic queue admission.
use alloc::{collections::BTreeSet, vec::Vec};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Vnc,
    Mahogany,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Absolute,
    Relative,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Key(u32, bool),
    Consumer(u32),
    Absolute(i16, i16),
    Relative(i16, i16),
    Button(i16, i16, u8),
    ButtonEdge(i16, i16, u8, bool),
    Scroll(i16, i16, i8),
    Reset,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Key(u32, bool),
    Pointer(u16, u16, u8),
    Move(i16, i16),
    Report(i16, i16, u8, Mode, i8),
    Mode(Mode),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    UnsupportedConsumer,
    InvalidButton,
    InvalidKey,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    protocol: Protocol,
    mode: Mode,
    keys: BTreeSet<u32>,
    x: i32,
    y: i32,
    buttons: u8,
}
impl Input {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            mode: Mode::Absolute,
            keys: BTreeSet::new(),
            x: 0,
            y: 0,
            buttons: 0,
        }
    }
    fn position(&mut self, x: i32, y: i32, width: u16, height: u16) {
        self.x = x.clamp(0, i32::from(width.saturating_sub(1)));
        self.y = y.clamp(0, i32::from(height.saturating_sub(1)));
    }
    fn select_mode(&mut self, mode: Mode, actions: &mut Vec<Action>) {
        if self.mode != mode {
            self.mode = mode;
            if self.protocol == Protocol::Mahogany {
                actions.push(Action::Mode(mode));
            }
        }
    }
    fn report(&self, x: i16, y: i16, wheel: i8, actions: &mut Vec<Action>) {
        match self.protocol {
            Protocol::Vnc => {
                for _ in 0..wheel.unsigned_abs().min(8) {
                    actions.push(Action::Pointer(
                        self.x as u16,
                        self.y as u16,
                        self.buttons | if wheel > 0 { 8 } else { 16 },
                    ));
                    actions.push(Action::Pointer(self.x as u16, self.y as u16, self.buttons));
                }
                if wheel == 0 {
                    actions.push(Action::Pointer(self.x as u16, self.y as u16, self.buttons));
                }
            }
            Protocol::Mahogany => {
                actions.push(Action::Report(x, y, self.buttons, self.mode, wheel))
            }
        }
    }
    pub fn step(
        mut self,
        event: Event,
        width: u16,
        height: u16,
    ) -> Result<(Self, Vec<Action>), Error> {
        let mut actions = Vec::new();
        match event {
            Event::Key(key, pressed) => {
                if pressed {
                    if key > u32::from(u16::MAX) {
                        return Err(Error::InvalidKey);
                    }
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
                actions.push(Action::Key(key, pressed));
            }
            Event::Consumer(key) => match self.protocol {
                Protocol::Vnc => return Err(Error::UnsupportedConsumer),
                Protocol::Mahogany => {
                    actions.push(Action::Key(key, true));
                    actions.push(Action::Key(key, false));
                }
            },
            Event::Absolute(x, y) => {
                self.select_mode(Mode::Absolute, &mut actions);
                self.position(i32::from(x), i32::from(y), width, height);
                self.report(x, y, 0, &mut actions);
            }
            Event::Relative(x, y) => {
                self.select_mode(Mode::Relative, &mut actions);
                self.position(self.x + i32::from(x), self.y + i32::from(y), width, height);
                match self.protocol {
                    Protocol::Vnc => self.report(0, 0, 0, &mut actions),
                    Protocol::Mahogany => actions.push(Action::Move(x, y)),
                }
            }
            Event::Button(x, y, buttons) => {
                if buttons & !7 != 0 {
                    return Err(Error::InvalidButton);
                }
                self.buttons = buttons;
                if self.mode == Mode::Absolute {
                    self.position(i32::from(x), i32::from(y), width, height);
                }
                self.report(x, y, 0, &mut actions);
            }
            Event::ButtonEdge(x, y, button, pressed) => {
                if button > 2 {
                    return Err(Error::InvalidButton);
                }
                if pressed {
                    self.buttons |= 1 << button;
                } else {
                    self.buttons &= !(1 << button);
                }
                if self.mode == Mode::Absolute {
                    self.position(i32::from(x), i32::from(y), width, height);
                }
                self.report(x, y, 0, &mut actions);
            }
            Event::Scroll(x, y, wheel) => {
                if wheel != 0 {
                    if self.mode == Mode::Absolute {
                        self.position(i32::from(x), i32::from(y), width, height);
                    }
                    self.report(x, y, wheel, &mut actions);
                }
            }
            Event::Reset => {
                for key in &self.keys {
                    actions.push(Action::Key(*key, false));
                }
                self.keys.clear();
                if self.protocol == Protocol::Mahogany {
                    actions.push(Action::Key(0, false));
                }
                self.buttons = 0;
                let (x, y) = match self.mode {
                    Mode::Absolute => (self.x as i16, self.y as i16),
                    Mode::Relative => (0, 0),
                };
                self.report(x, y, 0, &mut actions);
            }
        }
        Ok((self, actions))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_plan_keeps_all_keys_for_reset_retry() {
        let original = Input::new(Protocol::Vnc)
            .step(Event::Key(42, true), 640, 480)
            .unwrap()
            .0;
        let (planned, actions) = original.clone().step(Event::Reset, 640, 480).unwrap();
        assert!(actions.contains(&Action::Key(42, false)));
        assert_ne!(original, planned);
        assert_eq!(original.step(Event::Reset, 640, 480).unwrap().1, actions);
    }
    #[test]
    fn relative_vnc_coordinates_do_not_wrap_at_i16_boundary() {
        let state = Input::new(Protocol::Vnc)
            .step(Event::Absolute(32767, 0), 65535, 480)
            .unwrap()
            .0;
        let (_, actions) = state.step(Event::Relative(100, 0), 65535, 480).unwrap();
        assert_eq!(actions, [Action::Pointer(32867, 0, 0)]);
    }
    #[test]
    fn reset_and_consumer_are_bounded_batches() {
        for protocol in [Protocol::Vnc, Protocol::Mahogany] {
            let mut state = Input::new(protocol);
            for key in 0..24 {
                state = state.step(Event::Key(key, true), 640, 480).unwrap().0;
            }
            assert_eq!(
                state.clone().step(Event::Key(u32::MAX, true), 640, 480),
                Err(Error::InvalidKey)
            );
            assert_eq!(
                state.step(Event::Reset, 640, 480).unwrap().1.len(),
                if protocol == Protocol::Mahogany {
                    26
                } else {
                    25
                }
            );
        }
    }
}
