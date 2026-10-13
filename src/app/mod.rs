//! Application state (the reducer's data) plus the pure `reduce` function.

pub mod eventloop;
pub mod reducer;
pub mod state;
pub mod time;

pub use eventloop::{
    run_loop, CaptureDrawer, CrosstermInput, Drawer, Input, InputSource, ScriptedInput,
    TerminalDrawer, TICK,
};
pub use reducer::{msg_from_event, reduce, Effect, Msg, DEBOUNCE_MS, MAX_CONSECUTIVE_FAILURES};
pub use state::{
    AppState, Notice, NoticeLevel, Options, Phase, PlaybackState, ScanStatus, NOTICE_TTL,
};
pub use time::{now_ms, Clock, FakeClock, SystemClock};
