// ABOUTME: Defines telemetry frames and the non-blocking producer pipe.
// ABOUTME: Provides lightweight sysinfo polling used by the status monitor.

use std::sync::Arc;

use parking_lot::Mutex;
use triple_buffer::{Input, Output, TripleBuffer};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TelemetryFrame {
    pub cpu_total: f32,
    pub cpu_history: [f32; 60],
    pub net_activity_history: [f32; 60],
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub net_in_bytes_sec: u64,
    pub net_out_bytes_sec: u64,
    pub temp_celsius: f32,
}

impl Default for TelemetryFrame {
    fn default() -> Self {
        Self {
            cpu_total: 0.0,
            cpu_history: [0.0; 60],
            net_activity_history: [0.0; 60],
            mem_used_mb: 0,
            mem_total_mb: 0,
            net_in_bytes_sec: 0,
            net_out_bytes_sec: 0,
            temp_celsius: 0.0,
        }
    }
}

pub struct TelemetryPipe {
    producer: Input<TelemetryFrame>,
}

impl TelemetryPipe {
    pub fn new() -> (Self, Arc<Mutex<Output<TelemetryFrame>>>) {
        let (input, output) = TripleBuffer::new(TelemetryFrame::default()).split();
        (Self { producer: input }, Arc::new(Mutex::new(output)))
    }

    pub fn push(&mut self, frame: TelemetryFrame) {
        self.producer.write(frame);
    }
}

pub fn sample_frame() -> TelemetryFrame {
    let mut system = sysinfo::System::new_all();
    system.refresh_all();

    TelemetryFrame {
        cpu_total: system.global_cpu_info().cpu_usage(),
        mem_used_mb: system.used_memory() / 1024 / 1024,
        mem_total_mb: system.total_memory() / 1024 / 1024,
        ..TelemetryFrame::default()
    }
}
