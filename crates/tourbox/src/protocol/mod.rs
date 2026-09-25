//! デバイスとやり取りするバイト列の変換。I/O を持たない。

mod event;
mod frame;
mod haptics;

pub use event::{decode, Axis, Button, Direction, Event};
pub use frame::{NotAllowConfigDetector, NOT_ALLOW_CONFIG, UNLOCK};
pub use haptics::{HapticConfig, Modifier, Speed, Strength};
