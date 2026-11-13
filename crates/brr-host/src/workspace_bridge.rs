use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
pub type BrowserToolHandler = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;
