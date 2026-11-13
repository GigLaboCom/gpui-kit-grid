//! The current instrument, shared by every panel of an example: a list's
//! selection or a typed ticker sets it, and the other panels follow it.
//! The simulator reads it through `Control` to build the book, the tape and
//! the chart bars.

use std::sync::Arc;

use gpui_kit::{Context, EventEmitter};

use super::{instruments::InstrumentId, market::Control};

/// The current instrument changed.
#[derive(Clone, Copy, Debug)]
pub struct InstrumentChange {
    pub current: Option<InstrumentId>,
}

pub struct InstrumentBus {
    current: Option<InstrumentId>,
    control: Arc<Control>,
}

impl EventEmitter<InstrumentChange> for InstrumentBus {}

impl InstrumentBus {
    pub fn new(control: Arc<Control>) -> Self {
        Self {
            current: None,
            control,
        }
    }

    pub fn current(&self) -> Option<InstrumentId> {
        self.current
    }

    /// Makes `id` current: tells the simulator, emits `InstrumentChange`
    /// and notifies observers. The same instrument again does nothing.
    pub fn set_current(&mut self, id: Option<InstrumentId>, cx: &mut Context<Self>) {
        if self.current == id {
            return;
        }
        self.current = id;
        self.control.set_current(id);
        cx.emit(InstrumentChange { current: id });
        cx.notify();
    }
}
