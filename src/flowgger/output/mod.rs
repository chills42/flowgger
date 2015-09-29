mod debug_output;
mod kafka_output;
mod tls_output;

pub use self::debug_output::DebugOutput;
pub use self::kafka_output::KafkaOutput;
pub use self::tls_output::TlsOutput;

use flowgger::merger::Merger;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub trait Output {
    fn start(&self, arx: Arc<Mutex<Receiver<Vec<u8>>>>, merger: Option<Box<Merger>>);
}
