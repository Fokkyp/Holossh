//! GPUI 复用原子组件库（严格对应 Figma Page 2 组件与双模式变量）

pub mod primitives;
pub mod text_input;
pub mod toast;

pub use primitives::{
    boost_alpha, circular_progress_ring, current_ime_caret_bounds, glass_pill_button, hairline_h,
    icon_svg, module_loading_overlay, native_dropdown, native_toggle_with_id,
    set_active_ime_marked_text, status_dot,
};
pub use text_input::{
    TextCursorState, TextKeyOutcome, editable_text_line, hit_test_text_input,
    reset_caret_blink_epoch, sync_active_text_input_render_state,
};
pub use toast::{ToastItem, ToastKind, render_toast_stack};
