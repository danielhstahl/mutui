//! Application state (the reducer's data) plus the pure `reduce` function.

pub mod eventloop;
pub mod reducer;
pub mod state;
pub mod time;

pub use eventloop::{
    CaptureDrawer, CrosstermInput, Drawer, Input, InputSource, ScriptedInput, TICK, TerminalDrawer,
    run_loop,
};
pub use reducer::{DEBOUNCE_MS, Effect, MAX_CONSECUTIVE_FAILURES, Msg, msg_from_event, reduce};
pub use state::{
    AppState, NOTICE_TTL, Notice, NoticeLevel, Options, Phase, PlaybackState, ScanStatus,
};
pub use time::{Clock, FakeClock, SystemClock, now_ms};
