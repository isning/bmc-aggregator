use super::HidBackend;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(super) struct Keyboard {
    pub scancode: u16,
    #[serde(default = "pressed")]
    pub pressed: bool,
}
fn pressed() -> bool {
    true
}
#[derive(Debug, Deserialize)]
pub(super) struct Point {
    pub x: i16,
    pub y: i16,
}
#[derive(Debug, Deserialize)]
struct Scroll {
    x: i16,
    y: i16,
    scroll: i8,
}
#[derive(Debug, Deserialize)]
struct Buttons {
    x: i16,
    y: i16,
    buttons: u8,
}
#[derive(Debug, Deserialize)]
struct Transition {
    x: i16,
    y: i16,
    button: u8,
    pressed: bool,
}
#[derive(Debug, Deserialize)]
struct Consumer {
    usage: u16,
}

#[derive(Debug)]
pub(super) enum Input {
    Keyboard(Keyboard),
    Consumer(u16),
    Move(Point, bool),
    Scroll(i16, i16, i8),
    Buttons(i16, i16, u8),
    Transition(i16, i16, u8, bool),
}
impl Input {
    pub fn parse(action: &str, params: Value) -> Result<Self, String> {
        fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, String> {
            serde_json::from_value(params)
                .map_err(|_| "Invalid or out-of-range HID parameters".to_owned())
        }
        Ok(match action {
            "keyboard" => Self::Keyboard(decode(params)?),
            "consumer" => Self::Consumer(decode::<Consumer>(params)?.usage),
            "mouse_relative" => Self::Move(decode(params)?, false),
            "mouse" => {
                let absolute = match params.get("mode") {
                    None => false,
                    Some(Value::String(mode)) if mode == "relative" => false,
                    Some(Value::String(mode)) if mode == "absolute" => true,
                    _ => return Err("Invalid mouse mode".to_owned()),
                };
                Self::Move(decode(params)?, absolute)
            }
            "mouse_absolute" => Self::Move(decode(params)?, true),
            "scroll" => {
                let p: Scroll = decode(params)?;
                Self::Scroll(p.x, p.y, p.scroll)
            }
            "button" => {
                let p: Buttons = decode(params)?;
                if p.buttons > 7 {
                    return Err("Invalid mouse button mask".into());
                }
                Self::Buttons(p.x, p.y, p.buttons)
            }
            "button_transition" => {
                let p: Transition = decode(params)?;
                if p.button > 2 {
                    return Err("Invalid mouse button".into());
                }
                Self::Transition(p.x, p.y, p.button, p.pressed)
            }
            _ => return Err("Unknown HID action".into()),
        })
    }
    pub async fn execute(&self, backend: &dyn HidBackend) -> Result<(), String> {
        match self {
            Self::Keyboard(p) => backend.keyboard(p.scancode, p.pressed).await,
            Self::Consumer(usage) => backend.consumer(*usage).await,
            Self::Move(p, true) => backend.mouse_move_absolute(p.x, p.y).await,
            Self::Move(p, false) => backend.mouse_move_relative(p.x, p.y).await,
            Self::Scroll(x, y, delta) => backend.mouse_scroll(*x, *y, *delta).await,
            Self::Buttons(x, y, mask) => backend.mouse_button(*x, *y, *mask).await,
            Self::Transition(x, y, button, pressed) => {
                backend
                    .mouse_button_transition(*x, *y, *button, *pressed)
                    .await
            }
        }
    }
}

#[derive(Deserialize)]
struct BatchEvent {
    action: String,
    params: Value,
}
#[derive(Deserialize)]
struct Batch {
    events: Vec<BatchEvent>,
    #[serde(default = "delay")]
    delay_ms: u64,
}
fn delay() -> u64 {
    5
}
pub(super) fn batch(params: Value) -> Result<(Vec<Input>, u64), String> {
    let batch: Batch =
        serde_json::from_value(params).map_err(|_| "Invalid HID batch".to_owned())?;
    if batch.events.len() > 1024
        || batch.delay_ms > 1000
        || batch.delay_ms.saturating_mul(batch.events.len() as u64) > 30_000
    {
        return Err("HID batch exceeds its operation budget".into());
    }
    let events = batch
        .events
        .into_iter()
        .map(|event| Input::parse(&event.action, event.params))
        .collect::<Result<_, _>>()?;
    Ok((events, batch.delay_ms))
}

pub(super) fn screen_enabled(params: Value) -> Result<bool, String> {
    #[derive(Deserialize)]
    struct Screen {
        enabled: bool,
    }
    serde_json::from_value::<Screen>(params)
        .map(|params| params.enabled)
        .map_err(|_| "Invalid keyboard screen flag".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_invalid_input_before_any_batch_execution() {
        for (action, params) in [
            (
                "keyboard",
                serde_json::json!({"scancode":65540,"pressed":true}),
            ),
            ("mouse_absolute", serde_json::json!({"x":65536,"y":0})),
            ("scroll", serde_json::json!({"x":0,"y":0,"scroll":128})),
            ("button", serde_json::json!({"x":0,"y":0,"buttons":256})),
            (
                "keyboard",
                serde_json::json!({"scancode":4,"pressed":"yes"}),
            ),
            ("mouse", serde_json::json!({"x":0,"y":0,"mode":true})),
            ("mouse", serde_json::json!({"x":0,"y":0,"mode":42})),
            ("mouse", serde_json::json!({"x":0,"y":0,"mode":null})),
            ("unknown", serde_json::json!({})),
        ] {
            assert!(Input::parse(action, params).is_err());
        }
        assert!(batch(serde_json::json!({"events":[{"action":"keyboard","params":{"scancode":4}},{"action":"keyboard","params":{"scancode":65540}}]})).is_err());
        assert!(batch(serde_json::json!({"events":[{"action":"scroll","params":{"x":-32768,"y":32767,"scroll":-128}}],"delay_ms":0})).is_ok());
    }
}
