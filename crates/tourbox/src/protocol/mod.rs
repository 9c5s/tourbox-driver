//! デバイスとやり取りするバイト列の変換。I/O を持たない。

mod event;

pub use event::{decode, Axis, Button, Direction, Event};
