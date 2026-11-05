use super::pool::{AvctLease, HidCommand};
use async_trait::async_trait;
use bmc_feature_api::hid::{HidBackend, KeyboardLedCapabilities, KeyboardLedState};
use bmc_idrac_model::hid::{Hid, HidAction, HidEffect, HidEvent, PointerMode};
use tokio::sync::Mutex;

pub struct IdracAvctHidBackend {
    lease: AvctLease,
    intent: Mutex<Hid>,
}
impl IdracAvctHidBackend {
    pub fn new(lease: AvctLease) -> Self {
        Self {
            lease,
            intent: Mutex::new(Hid::default()),
        }
    }
    async fn dispatch(&self, event: HidEvent) -> Result<(), String> {
        // One owner serializes planning, output, and intent commit. Retain old
        // intent on failure/cancellation so reset can retry all releases.
        let mut intent = self.intent.lock().await;
        let transition = intent.clone().step(event);
        let HidEffect::Send(actions) = transition.effect else {
            return Err(
                "iDRAC AVCT supports left, middle, and right mouse buttons only".to_string(),
            );
        };
        for action in actions {
            let session = self.lease.session();
            match action {
                HidAction::Key { code, pressed } => {
                    session
                        .send_hid(HidCommand::Keyboard {
                            scancode: code,
                            pressed,
                        })
                        .await?
                }
                HidAction::Consumer(usage) => {
                    session.send_hid(HidCommand::Consumer { usage }).await?
                }
                HidAction::Pointer {
                    x,
                    y,
                    buttons,
                    scroll,
                    mode: PointerMode::Relative,
                } => {
                    session
                        .send_relative_pointer_with_scroll(x, y, buttons, scroll)
                        .await?
                }
                HidAction::Pointer {
                    x,
                    y,
                    buttons,
                    scroll,
                    mode: PointerMode::Absolute,
                } => {
                    let command = if scroll == 0 {
                        HidCommand::MouseButton { x, y, buttons }
                    } else {
                        HidCommand::MouseScroll {
                            x,
                            y,
                            buttons,
                            scroll,
                        }
                    };
                    session.send_hid(command).await?
                }
            }
        }
        *intent = transition.state;
        Ok(())
    }
}
#[async_trait]
impl HidBackend for IdracAvctHidBackend {
    async fn keyboard_led_capabilities(&self) -> KeyboardLedCapabilities {
        KeyboardLedCapabilities {
            reports_state: false,
        }
    }
    async fn keyboard_led_state(&self) -> Result<KeyboardLedState, String> {
        Ok(KeyboardLedState::default())
    }
    async fn keyboard(&self, scancode: u16, pressed: bool) -> Result<(), String> {
        self.dispatch(HidEvent::Key {
            code: scancode,
            pressed,
        })
        .await
    }
    async fn consumer(&self, usage: u16) -> Result<(), String> {
        self.dispatch(HidEvent::Consumer(usage)).await
    }
    async fn mouse_move(&self, x: i16, y: i16) -> Result<(), String> {
        self.mouse_move_absolute(x, y).await
    }
    async fn mouse_move_absolute(&self, x: i16, y: i16) -> Result<(), String> {
        self.dispatch(HidEvent::Move {
            x,
            y,
            mode: PointerMode::Absolute,
        })
        .await
    }
    async fn mouse_move_relative(&self, x: i16, y: i16) -> Result<(), String> {
        self.dispatch(HidEvent::Move {
            x,
            y,
            mode: PointerMode::Relative,
        })
        .await
    }
    async fn mouse_button(&self, x: i16, y: i16, buttons: u8) -> Result<(), String> {
        self.dispatch(HidEvent::Buttons { x, y, buttons }).await
    }
    async fn mouse_button_transition(
        &self,
        x: i16,
        y: i16,
        button: u8,
        pressed: bool,
    ) -> Result<(), String> {
        self.dispatch(HidEvent::Button {
            x,
            y,
            button,
            pressed,
        })
        .await
    }
    async fn mouse_scroll(&self, x: i16, y: i16, scroll: i8) -> Result<(), String> {
        self.dispatch(HidEvent::Scroll { x, y, scroll }).await
    }
    async fn reset(&self) -> Result<(), String> {
        self.dispatch(HidEvent::Reset).await
    }
}
