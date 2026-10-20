use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

impl SseEvent {
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    pub fn to_wire_string(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.event, self.data)
    }
}
