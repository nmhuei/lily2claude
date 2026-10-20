pub mod handlers;
pub mod routes;
pub mod state;

pub use crate::protocol::prompt::build_acp_prompt;
pub use routes::create_router;
pub use state::AppState;

pub fn get_streamable_text_end(s: &str) -> usize {
    crate::protocol::repair::ToolCallRepairEngine::get_streamable_text_end(s)
}
